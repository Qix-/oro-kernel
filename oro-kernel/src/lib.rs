//! Kernel for the [Oro Operating System](https://github.com/oro-os/kernel).
//!
//! This crate is a library with the core kernel functionality, datatypes,
//! etc. and provides a common interface for architectures to implement
//! the Oro kernel on their respective platforms.
#![no_std]
// NOTE(qix-): `adt_const_params` isn't strictly necessary but is on track for acceptance,
// NOTE(qix-): and the open questions (e.g. mangling) are not of concern here.
// NOTE(qix-): https://github.com/rust-lang/rust/issues/95174
#![feature(adt_const_params)]

use oro_macro::assert;
use oro_mem::{
	mapper::{AddressSegment, AddressSpace, MapError},
	pfa::alloc::Alloc,
	translate::Translator,
};
use oro_sync::spinlock::unfair_critical::{InterruptController, UnfairCriticalSpinlock};

pub mod id;
pub mod instance;
pub mod module;
pub mod port;
pub mod registry;
pub mod ring;
pub mod thread;

/// Core-local instance of the Oro kernel.
///
/// This object's constructor sets up a core-local
/// mapping of itself such that it can be accessed
/// from anywhere in the kernel as a static reference.
pub struct Kernel<Pfa, Pat, AddrSpace, IntCtrl>
where
	Pfa: Alloc + 'static,
	Pat: Translator,
	AddrSpace: AddressSpace + 'static,
	IntCtrl: InterruptController + 'static,
{
	/// Global reference to the shared kernel state.
	state: &'static KernelState<Pfa, Pat, AddrSpace, IntCtrl>,
}

impl<Pfa, Pat, AddrSpace, IntCtrl> Kernel<Pfa, Pat, AddrSpace, IntCtrl>
where
	Pfa: Alloc + 'static,
	Pat: Translator,
	AddrSpace: AddressSpace,
	IntCtrl: InterruptController,
{
	/// Initializes a new core-local instance of the Oro kernel.
	///
	/// The [`AddressSpace::kernel_core_local()`] segment must
	/// be empty prior to calling this function, else it will
	/// return [`MapError::Exists`].
	///
	/// # Safety
	/// Must only be called once per CPU session (i.e.
	/// boot or bringup after a powerdown case, where the
	/// previous core-local [`Kernel`] was migrated or otherwise
	/// destroyed).
	///
	/// The `state` given to the kernel must be shared for all
	/// instances of the kernel that wish to partake in the same
	/// Oro kernel universe.
	pub unsafe fn initialize_for_core(
		state: &'static KernelState<Pfa, Pat, AddrSpace, IntCtrl>,
	) -> Result<&'static Self, MapError> {
		assert::fits::<Self, 4096>();

		let mapper = AddrSpace::current_supervisor_space(&state.pat);
		let core_local_segment = AddrSpace::kernel_core_local();

		let kernel_base = core_local_segment.range().0;
		debug_assert!((kernel_base as *mut Self).is_aligned());

		{
			let mut pfa = state.pfa.lock::<IntCtrl>();
			let phys = pfa.allocate().ok_or(MapError::OutOfMemory)?;
			core_local_segment.map(&mapper, &mut *pfa, &state.pat, kernel_base, phys)?;
		}

		let kernel_ptr = kernel_base as *mut Self;
		kernel_ptr.write(Self { state });

		Ok(&*kernel_ptr)
	}

	/// Returns a reference to the core-local kernel instance.
	///
	/// # Assumed Safety
	/// This function is not marked `unsafe` since, under pretty much
	/// every circumstance, the kernel instance should be initialized
	/// for the core before any other code runs. If this function is
	/// called before the kernel is initialized, undefined behavior
	/// will occur.
	///
	/// Architectures **must** make sure [`Self::initialize_for_core()`]
	/// has been called as soon as possible after the core boots.
	#[must_use]
	pub fn get() -> &'static Self {
		// SAFETY(qix-): The kernel instance is initialized for the core
		// SAFETY(qix-): before any other code runs.
		unsafe { &*(AddrSpace::kernel_core_local().range().0 as *const Self) }
	}

	/// Returns the underlying [`KernelState`] for this kernel instance.
	#[must_use]
	pub fn state(&self) -> &'static KernelState<Pfa, Pat, AddrSpace, IntCtrl> {
		self.state
	}
}

/// Global state shared by all [`Kernel`] instances across
/// core boot/powerdown/bringup cycles.
pub struct KernelState<Pfa, Pat, AddrSpace, IntCtrl>
where
	Pfa: Alloc,
	Pat: Translator,
	AddrSpace: AddressSpace,
	IntCtrl: InterruptController,
{
	/// The shared, spinlocked page frame allocator (PFA) for the
	/// entire system.
	pfa:           UnfairCriticalSpinlock<Pfa>,
	/// The physical address translator.
	pat:           Pat,
	/// Ring registry.
	ring_registry: registry::Registry<ring::Ring, IntCtrl, AddrSpace, Pat>,
}

impl<Pfa, Pat, AddrSpace, IntCtrl> KernelState<Pfa, Pat, AddrSpace, IntCtrl>
where
	Pfa: Alloc,
	Pat: Translator,
	AddrSpace: AddressSpace,
	IntCtrl: InterruptController,
{
	/// Creates a new instance of the kernel state. Meant to be called
	/// once for all cores at boot time.
	///
	/// # Safety
	/// This function must ONLY be called by the primary core,
	/// only at boot time, and only before any other cores are brought up,
	/// exactly once.
	///
	/// This function sets up shared page table mappings that MUST be
	/// shared across cores. The caller MUST initialize the kernel
	/// state (this struct) prior to booting _any other cores_
	/// or else registry accesses will page fault.
	#[allow(clippy::missing_panics_doc)]
	pub unsafe fn new(pat: Pat, pfa: UnfairCriticalSpinlock<Pfa>) -> Result<Self, MapError> {
		let ring_registry = {
			let mut pfa_lock = pfa.lock::<IntCtrl>();

			let reg = registry::Registry::new(
				pat.clone(),
				&mut *pfa_lock,
				AddrSpace::kernel_ring_registry(),
			)?;

			let _ = pfa_lock;

			reg
		};

		let root_ring_id = ring_registry.insert_permanent(
			&pfa,
			ring::Ring {
				id:     0,
				parent: None,
			},
		)?;
		assert_eq!(root_ring_id, 0, "root ring ID must be 0");

		Ok(Self {
			pfa,
			pat,
			ring_registry,
		})
	}

	/// Returns the underlying PFA belonging to the kernel state.
	pub fn pfa(&'static self) -> &'static UnfairCriticalSpinlock<Pfa> {
		&self.pfa
	}

	/// Returns a [`registry::Handle`] to a [`ring::Ring`] by its ID,
	/// or `None` if it does not exist.
	///
	/// # Safety
	/// **DO NOT USE THIS FUNCTION FOR ANYTHING SECURITY RELATED.**
	///
	/// IDs are re-used by registries when items are dropped, so
	/// multiple calls to this function with the same ID may return
	/// handles to different ring items as the IDs get recycled.
	///
	/// In almost all cases, you should be using [`registry::Handle`]s
	/// directly. They are also easier to work with than calling
	/// this function.
	pub unsafe fn ring_by_id(&'static self, id: usize) -> Option<registry::Handle<ring::Ring>> {
		self.ring_registry.get(id)
	}

	/// Inserts a [`ring::Ring`] into the registry and returns
	/// its [`registry::Handle`].
	///
	/// `ring.id` will be set to the allocated ID, and is ignored
	/// when passed in.
	///
	/// Note that the returned handle is reference counted; dropping
	/// it will drop the ring from the registry. If the ring is
	/// intended to be kept alive, it should be added to a scheduler.
	pub fn insert_ring(
		&'static self,
		ring: ring::Ring,
	) -> Result<registry::Handle<ring::Ring>, MapError> {
		let handle = self.ring_registry.insert(&self.pfa, ring)?;
		unsafe {
			handle.lock::<IntCtrl>().id = handle.id();
		}
		Ok(handle)
	}
}
