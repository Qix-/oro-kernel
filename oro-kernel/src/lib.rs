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

pub mod id;
pub mod instance;
pub mod module;
pub mod port;
pub mod registry;
pub mod ring;
pub mod scheduler;
pub mod thread;

use self::{
	registry::{Handle, List, ListRegistry, Registry},
	scheduler::Scheduler,
};
use core::mem::MaybeUninit;
use oro_macro::assert;
use oro_mem::{
	mapper::{AddressSegment, AddressSpace, MapError},
	pfa::alloc::Alloc,
	translate::Translator,
};
use oro_sync::spinlock::unfair_critical::{InterruptController, UnfairCriticalSpinlock};

/// Core-local instance of the Oro kernel.
///
/// This object's constructor sets up a core-local
/// mapping of itself such that it can be accessed
/// from anywhere in the kernel as a static reference.
pub struct Kernel<CoreState: Sized + 'static, A: Arch> {
	/// Local core state. The kernel instance owns this
	/// due to all of the machinery already in place to make
	/// this kernel instance object core-local and accessible
	/// from anywhere in the kernel.
	core_state: CoreState,
	/// Global reference to the shared kernel state.
	state:      &'static KernelState<A>,
	/// The kernel scheduler
	scheduler:  Scheduler<A>,
}

impl<CoreState: Sized + 'static, A: Arch> Kernel<CoreState, A> {
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
		global_state: &'static KernelState<A>,
		core_state: CoreState,
	) -> Result<&'static Self, MapError> {
		assert::fits::<Self, 4096>();

		let mapper = AddrSpace::<A>::current_supervisor_space(&global_state.pat);
		let core_local_segment = AddrSpace::<A>::kernel_core_local();

		let kernel_base = core_local_segment.range().0;
		debug_assert!((kernel_base as *mut Self).is_aligned());

		{
			let mut pfa = global_state.pfa.lock::<A::IntCtrl>();
			let phys = pfa.allocate().ok_or(MapError::OutOfMemory)?;
			core_local_segment.map(&mapper, &mut *pfa, &global_state.pat, kernel_base, phys)?;
		}

		let kernel_ptr = kernel_base as *mut Self;
		kernel_ptr.write(Self {
			core_state,
			state: global_state,
			scheduler: Scheduler::new(),
		});

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
		unsafe { &*(AddrSpace::<A>::kernel_core_local().range().0 as *const Self) }
	}

	/// Returns the underlying [`KernelState`] for this kernel instance.
	#[must_use]
	pub fn state(&self) -> &'static KernelState<A> {
		self.state
	}

	/// Returns the architecture-specific core local state reference.
	#[must_use]
	pub fn core(&self) -> &CoreState {
		&self.core_state
	}

	/// Gets a reference to the scheduler.
	#[must_use]
	pub fn scheduler(&self) -> &Scheduler<A> {
		&self.scheduler
	}
}

/// Global state shared by all [`Kernel`] instances across
/// core boot/powerdown/bringup cycles.
pub struct KernelState<A: Arch> {
	/// The shared, spinlocked page frame allocator (PFA) for the
	/// entire system.
	pfa: UnfairCriticalSpinlock<A::Pfa>,
	/// The physical address translator.
	pat: A::Pat,

	/// List of all modules.
	///
	/// Always `Some` after a valid initialization.
	/// Can be safely `.unwrap()`'d in most situations.
	modules:   Option<Handle<List<module::Module, A>>>,
	/// List of all rings.
	///
	/// Always `Some` after a valid initialization.
	/// Can be safely `.unwrap()`'d in most situations.
	rings:     Option<Handle<List<ring::Ring<A>, A>>>,
	/// The root ring.
	///
	/// Always `Some` after a valid initialization.
	/// Can be safely `.unwrap()`'d in most situations.
	root_ring: Option<Handle<ring::Ring<A>>>,

	/// Ring registry.
	ring_registry:          Registry<ring::Ring<A>, A>,
	/// Ring list registry.
	ring_list_registry:     ListRegistry<ring::Ring<A>, A>,
	/// Module registry.
	#[expect(dead_code)]
	module_registry:        Registry<module::Module, A>,
	/// Module list registry.
	module_list_registry:   ListRegistry<module::Module, A>,
	/// Instance registry.
	#[expect(dead_code)]
	instance_registry:      Registry<instance::Instance<A>, A>,
	/// Instance list registry.
	instance_list_registry: ListRegistry<instance::Instance<A>, A>,
	/// Thread registry.
	#[expect(dead_code)]
	thread_registry:        Registry<thread::Thread<A>, A>,
	/// Thread list registry.
	#[expect(dead_code)]
	thread_list_registry:   ListRegistry<thread::Thread<A>, A>,
	/// Port registry.
	#[expect(dead_code)]
	port_registry:          Registry<port::Port, A>,
	/// Port list registry.
	#[expect(dead_code)]
	port_list_registry:     ListRegistry<port::Port, A>,
}

impl<A: Arch> KernelState<A> {
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
	pub unsafe fn init(
		this: &'static mut MaybeUninit<Self>,
		pat: A::Pat,
		pfa: UnfairCriticalSpinlock<A::Pfa>,
	) -> Result<(), MapError> {
		#[expect(clippy::missing_docs_in_private_items)]
		macro_rules! init_registries {
			($($id:ident => $(($listregfn:ident, $itemregfn:ident))? $($regfn:ident)?),* $(,)?) => {
				$(
					let $id = {
						let mut pfa_lock = pfa.lock::<A::IntCtrl>();
						let reg = init_registries!(@inner pfa_lock, $(($listregfn, $itemregfn))? $($regfn)?);
						let _ = pfa_lock;

						reg
					};
				)*
			};

			(@inner $pfa_lock:expr, ( $listregfn:ident , $itemregfn:ident )) => {
				ListRegistry::new(
					pat.clone(),
					&mut *$pfa_lock,
					A::AddrSpace::$listregfn(),
					A::AddrSpace::$itemregfn(),
				)?
			};

			(@inner $pfa_lock:expr, $regfn:ident) => {
				Registry::new(
					pat.clone(),
					&mut *$pfa_lock,
					A::AddrSpace::$regfn(),
				)?
			};
		}

		init_registries! {
			ring_registry => kernel_ring_registry,
			ring_list_registry => (kernel_ring_list_registry, kernel_ring_item_registry),
			module_registry => kernel_module_registry,
			module_list_registry => (kernel_module_list_registry, kernel_module_item_registry),
			instance_registry => kernel_instance_registry,
			instance_list_registry => (kernel_instance_list_registry, kernel_instance_item_registry),
			thread_registry => kernel_thread_registry,
			thread_list_registry => (kernel_thread_list_registry, kernel_thread_item_registry),
			port_registry => kernel_port_registry,
			port_list_registry => (kernel_port_list_registry, kernel_port_item_registry),
		}

		this.write(Self {
			pfa,
			pat,
			root_ring: None,
			modules: None,
			rings: None,
			ring_registry,
			ring_list_registry,
			module_registry,
			module_list_registry,
			instance_registry,
			instance_list_registry,
			thread_registry,
			thread_list_registry,
			port_registry,
			port_list_registry,
		});

		let this = this.assume_init_mut();

		let root_ring = this.ring_registry.insert(
			&this.pfa,
			ring::Ring {
				id:        0,
				parent:    None,
				instances: this.instance_list_registry.create_list(&this.pfa)?,
			},
		)?;
		assert_eq!(root_ring.id(), 0, "root ring ID must be 0");

		let modules = this.module_list_registry.create_list(&this.pfa)?;
		let rings = this.ring_list_registry.create_list(&this.pfa)?;

		let _ = rings.append(&this.pfa, root_ring.clone())?;

		this.root_ring = Some(root_ring);
		this.rings = Some(rings);
		this.modules = Some(modules);

		Ok(())
	}

	/// Returns the underlying PFA belonging to the kernel state.
	pub fn pfa(&'static self) -> &'static UnfairCriticalSpinlock<A::Pfa> {
		&self.pfa
	}

	/// Creates a new ring and returns a [`registry::Handle`] to it.
	#[expect(clippy::missing_panics_doc)]
	pub fn create_ring(
		&'static self,
		parent: Handle<ring::Ring<A>>,
	) -> Result<Handle<ring::Ring<A>>, MapError> {
		let ring = self.ring_registry.insert(
			&self.pfa,
			ring::Ring::<A> {
				id:        usize::MAX, // placeholder
				parent:    Some(parent),
				instances: self.instance_list_registry.create_list(&self.pfa)?,
			},
		)?;

		// SAFETY(qix-): Will not panic.
		unsafe {
			ring.lock::<A::IntCtrl>().id = ring.id();
		}

		// SAFETY(qix-): As long as the kernel state has been initialized,
		// SAFETY(qix-): this won't panic.
		let _ = self.rings.as_ref().unwrap().append(&self.pfa, ring.clone());

		Ok(ring)
	}
}

/// A trait for architectures to list commonly used types
/// to be passed around the kernel.
pub trait Arch: 'static {
	/// The physical address translator (PAT) the architecture
	/// uses.
	type Pat: Translator;
	/// The type of page frame allocator (PFA) the architecture
	/// uses.
	type Pfa: Alloc;
	/// The type of interrupt controller.
	type IntCtrl: InterruptController;
	/// The address space layout the architecture uses.
	type AddrSpace: AddressSpace;
}

/// Helper trait association type for `Arch::AddrSpace`.
pub(crate) type AddrSpace<A> = <A as Arch>::AddrSpace;
/// Helper trait association type for `Arch::AddrSpace::SupervisorSegment`.
pub(crate) type SupervisorSegment<A> = <AddrSpace<A> as AddressSpace>::SupervisorSegment;
/// Helper trait association type for `Arch::AddrSpace::SupervisorHandle`.
pub(crate) type SupervisorHandle<A> = <AddrSpace<A> as AddressSpace>::SupervisorHandle;
/// Helper trait association type for `Arch::AddrSpace::UserHandle`.
pub(crate) type UserHandle<A> = <AddrSpace<A> as AddressSpace>::UserHandle;
