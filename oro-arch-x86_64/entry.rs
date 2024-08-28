//! Main entry point for the Oro Kernel on the x86_64 architecture.
#![no_std]
#![no_main]

/// Panic handler for the kernel.
#[inline(never)]
#[cold]
#[panic_handler]
unsafe fn panic(_info: &::core::panic::PanicInfo) -> ! {
	<oro_arch_x86_64::X86_64 as oro_common::arch::Arch>::halt();
}

/// Main entry point for the Oro kernel. Bootloaders jump
/// to this function to start the kernel.
///
/// # Safety
/// Do **NOT** call this function directly. It should be
/// treated as an ELF entry point and jumped to by the
/// bootloader.
#[inline(never)]
#[cold]
#[no_mangle]
#[allow(clippy::missing_panics_doc, irrefutable_let_patterns)] // XXX(qix-) DEBUG
pub unsafe extern "C" fn _start() -> ! {
	::oro_arch_x86_64::boot::boot_primary();
}
