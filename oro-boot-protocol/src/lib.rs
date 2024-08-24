//! # Oro Boot Protocol
//! The Oro kernel boot protocol is a standardized interface for
//! booting into the Oro kernel.
//!
//! This crate provides all necessary types and documentation
//! for booting into the oro kernel from any environment, and
//! provides C headers for doing so from languages other than
//! Rust.
//!
//! The Oro boot protocol is heavily inspired by the Limine protocol
//! in that it uses versioned, tagged structures that are scanned for
//! and populated in the kernel address space after it is mapped.
//!
//! This crate documents the exact means by which the kernel's
//! protocol tags should be searched for and used.
//!
//! Users who wish to use a higher-level API to boot into the Oro
//! kernel should use the `oro-boot` crate, which provides a
//! safe and standardized interface for booting into the Oro kernel
//! without the need to implement the boot protocol directly.
//!
//! # Overview of Tag System
//! The boot protocol is based on request-response model, whereby
//! the kernel exports requests aligned on a 16-bit boundary somewhere
//! in the kernel address space. The bootloader is expected to scan
//! for these requests and populate them with the necessary data.
//!
//! Note that tags are architecture-endian, meaning the kernel
//! compiled for a little-endian system will have its tag bytes
//! reversed when compared to a kernel compiled for a big-endian
//! system.
//!
//! All discovered tags are expected to be populated, except for
//! those that are explicitly marked as optional.
//!
//! If a bootloader fails to populate a tag, the kernel is allowed
//! to panic if it cannot continue without it.
//!
//! # Discovering Requests
//! The Oro kernel ships as an ELF executable with a number of
//! program headers. One of the headers that is present is a
//! read-only header with the OS-specific flag `(1 << 21)`.
//!
//! In addition, all Kernel program headers will have the Oro
//! Kernel bit raised - `(1 << 20)`. This is to help prevent
//! bad user configuration from attempting to load a normal
//! ELF executable as a kernel.
//!
//! Once the segment is located, the bootloader is expected to
//! scan, on 16-byte boundaries, for tags (see the individual
//! requests' `TAG` constant, e.g. [`KernelSettingsRequest::TAG`]).
//!
//! The base address of the found tag is in turn the base address
//! of the [`RequestHeader`] structure, which is guaranteed to be
//! present at the beginning of every request.
//!
//! The request header is then used to determine the revision of
//! the request, and the appropriate data structure to populate.
//! If the bootloader does not recognize the revision, it should
//! skip the tag.
//!
//! The data directly after the request header is the data structure
//! to populate.
//!
//! # Populating Requests
//! The bootloader is expected to populate the request with the
//! appropriate data. The kernel will then use this data to
//! configure itself.
//!
//! The data should be mapped into the kernel's memory as specified
//! by the program header, with read-only permissions (thus the bootloader
//! must edit the memory prior to setting up the kernel's page tables).
//!
//! Upon populating a request, its `populated` value must be set to
//! `0xFF`. Bootloaders _should_ first make sure that the value was
//! `0x00` before populating the request, as a sanity check that
//! some bug or corruption did not occur.
#![cfg_attr(not(test), no_std)]
#![deny(
	missing_docs,
	clippy::integer_division,
	clippy::missing_docs_in_private_items
)]
#![allow(clippy::too_many_lines)] // Seems to be a bug in clippy with the macro expansion

mod macros;
#[cfg(feature = "utils")]
pub mod util;

/// The type of the kernel request tag.
pub type Tag = u64;

macros::oro_boot_protocol! {
	/// Main settings for the kernel.
	b"ORO_KRNL" => KernelSettings {
		0 => {
			/// The virtual offset of the linear map of physical memory.
			pub linear_map_offset: usize,
		}
	}

	/// A request for the memory map.
	b"ORO_MMAP" => MemoryMap {
		0 => {
			/// The number of entries in the memory map.
			pub entry_count: u64,
			/// The memory map entries.
			#[vla(entry_count)]
			pub entries: [MemoryMapEntry; 0],
		}
	}

	/// A request for CPU core information.
	b"ORO_CPUS" => Cpus {
		0 => {
			/// The ID of the BSP (bootstrap) core.
			///
			/// The ID doesn't need to be 0 nor 1, but it must be unique
			/// among all core IDs.
			///
			/// # x86_64
			/// On x86_64, this is the APIC ID of the core.
			///
			/// # AArch64
			/// On AArch64, this is the MPIDR of the core.
			pub bsp_id: u64,
			/// The number of CPUs being booted, **EXCLUDING** the BSP
			/// (primary / bootstrap processor) core.
			///
			/// For example, if this is a single core system, this value is
			/// `0`, if this is a dual-core system, this value is `1`, etc.
			///
			/// `entries` must contain all secondary core information.
			pub num_cpus: u32,
			/// The CPU core information entries.
			#[vla(num_cpus)]
			pub entries: [SecondaryCpu; 0],
		}
	}

	/// **THIS IS TEMPORARY AND WILL BE REMOVED.**
	///
	/// Temporary request for the PFA head. This is to be removed
	/// after the kernel boot sequence is refactored.
	b"ORO_PFAH" => PfaHead {
		0 => {
			/// The physical address of the PFA head.
			pub pfa_head: u64,
		}
	}
}

/// A memory map entry, representing a chunk of physical memory
/// available to the system.
#[repr(C)]
#[derive(Debug, Clone)]
pub struct MemoryMapEntry {
	/// The base address of the memory region.
	pub base:   u64,
	/// The length of the memory region.
	pub length: usize,
	/// The type of the memory region.
	pub ty:     MemoryMapEntryType,
}

impl PartialOrd for MemoryMapEntry {
	fn partial_cmp(&self, other: &Self) -> Option<::core::cmp::Ordering> {
		self.base.partial_cmp(&other.base)
	}
}

impl PartialEq for MemoryMapEntry {
	fn eq(&self, other: &Self) -> bool {
		self.base == other.base
	}
}

/// The type of a memory map entry.
///
/// For any unknown types, the bootloader should specify
/// [`MemoryMapEntryType::Unknown`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C)]
pub enum MemoryMapEntryType {
	/// Memory that is either unusable or reserved, or some type
	/// of memory that is available to the system but not any
	/// specific type usable by the kernel.
	Unknown           = 0,
	/// General memory immediately usable by the kernel.
	Usable            = 1,
	/// Memory that is used by the bootloader but that can be
	/// reclaimed by the kernel.
	BootloaderReclaim = 2,
	/// Memory that holds either the kernel itself, root ring modules,
	/// or other boot-time binary data (e.g. `DeviceTree` blobs).
	///
	/// This memory is not reclaimed nor written to by the kernel.
	Modules           = 3,
	/// Bad memory. This memory is functionally equivalent to
	/// `Unknown`, but is used to denote memory that is known to
	/// be bad, broken, or malfunctioning. It is reported to the user
	/// as such.
	Bad               = 4,
}

/// A secondary CPU core.
#[repr(C)]
#[derive(Debug, Clone)]
pub struct SecondaryCpu {
	/// The ID of the CPU core.
	///
	/// # x86_64
	/// On x86_64, this is the APIC ID of the core.
	///
	/// # AArch64
	/// On AArch64, this is the MPIDR of the core.
	pub id:    u64,
	/// The entry point of the CPU core. The kernel
	/// will perform a volatile write to this address
	/// to wake the core.
	pub entry: usize,
}
