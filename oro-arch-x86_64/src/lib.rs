//! x86_64 architecture support crate for the
//! [Oro Operating System](https://github.com/oro-os/kernel)
//! kernel.
//!
//! # Safety
//! To support x86_64 when implementing a preboot stage, please read
//! **both** [`oro_common::boot_to_kernel`]'s documentation as well the
//! following safety requirements **carefully**.
//!
//! ## Memory
//! There are a few memory requirements that the x86_64 architecture support
//! mandates:
//!
//! ### Direct Maps
//! The Oro x86_64 architecture assumes a direct map of all physical memory
//! is direct mapped into the the address space. The implementation of a
//! [`oro_common::mem::PhysicalAddressTranslator`] is required to map physical
//!  addresses to virtual addresses in a deterministic fashion.
//!
//! While the memory regions do not technically need to be offset-based, it's
//! highly recommended to do so for ease of implementation. The common library
//! provides an [`oro_common::mem::OffsetPhysicalAddressTranslator`] that can
//! be used if a simple offset needs to be applied to the physical address
//! to form a virtual address.
//!
//! ### Higher-Half Mapping
//! The Oro x86_64 architecture assumes the lower half of the
//! address space is "free game" during the preboot (bootloader)
//! stage. This is required for the execution handoff to the kernel,
//! whereby some common stubs are mapped both into the target kernel
//! address space as well as the current address space such that
//! the execution can resume after page tables are switched out.
//!
//! If possible, higher-half direct maps are advised. If not possible,
//! attempt to direct-map in the lower quarter of the address space
//! to avoid conflicts with the stub mappings. Stubs are mapped into
//! L4/L5 index 255, but this is NOT a stable guarantee.
//!
//! ### Shared Page Tables
//! The Oro x86_64 architecture expects that all SMP cores invoking
//! [`oro_common::boot_to_kernel`] use a shared page table - that is,
//! the `cr3` register points to the same base address for all cores.
#![no_std]
#![deny(
	missing_docs,
	clippy::integer_division,
	clippy::missing_docs_in_private_items
)]
#![allow(
	internal_features,
	clippy::verbose_bit_mask,
	clippy::module_name_repetitions
)]
#![feature(
	const_mut_refs,
	naked_functions,
	core_intrinsics,
	debug_closure_helpers
)]
#![cfg(not(all(doc, not(target_arch = "x86_64"))))]

#[cfg(debug_assertions)]
pub(crate) mod dbgutil;

pub(crate) mod arch;
pub(crate) mod asm;
pub(crate) mod mem;
pub(crate) mod xfer;

pub use self::arch::{init_kernel_primary, init_kernel_secondary, init_preboot_primary, X86_64};
