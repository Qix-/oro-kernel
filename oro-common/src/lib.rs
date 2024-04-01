//! Common code and utilities for crates within
//! the [Oro Operating System](https://github.com/oro-os/kernel)
//! kernel project.
//!
//! # Bootloaders
//! If you are implementing a bootloader and want to boot into
//! the Oro kernel, see the [`boot_to_kernel`] function.
//!
//! # Architectures
//! If you are implementing an architecture for Oro, see the
//! [`Arch`] trait.
#![no_std]
#![deny(
	missing_docs,
	clippy::integer_division,
	clippy::missing_docs_in_private_items
)]
#![allow(clippy::module_name_repetitions)]
#![feature(const_trait_impl)]
#![cfg_attr(feature = "unstable", feature(core_intrinsics))]
#![cfg_attr(feature = "unstable", allow(internal_features))]

pub mod mem;
pub mod sync;

pub(crate) mod arch;
pub(crate) mod dbg;
pub(crate) mod init;
pub(crate) mod macros;
pub(crate) mod unsafe_macros;

pub use self::{
	arch::Arch,
	init::{boot_to_kernel, PrebootConfig, PrebootPrimaryConfig},
};
