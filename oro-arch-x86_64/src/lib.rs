//! x86_64 architecture support crate for the
//! [Oro Operating System](https://github.com/oro-os/kernel)
//! kernel.
#![no_std]
#![deny(missing_docs)]

use core::arch::asm;
use oro_common::Arch;

/// x86_64 architecture support implementation for the Oro kernel.
pub struct X86_64;

impl Arch for X86_64 {
	unsafe fn init() {}

	fn halt() -> ! {
		loop {
			unsafe {
				asm!("cli", "hlt");
			}
		}
	}
}
