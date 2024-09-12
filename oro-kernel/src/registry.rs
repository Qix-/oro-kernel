//! Implements types for the Oro registries (reference-counted
//! arena allocators).

// NOTE(qix-): This is an **INCREDIBLY UNSAFE** module. It is designed
// NOTE(qix-): to be as ergonomic and safe as possible from the outset,
// NOTE(qix-): especially for how it'll be used within Oro, but it employs
// NOTE(qix-): some normaally very questionable practices to achieve this
// NOTE(qix-): in order to stay performant in the hot path (optimizing
// NOTE(qix-): reads over writes, minimizing locking whilst doing so,
// NOTE(qix-): for example).
// NOTE(qix-):
// NOTE(qix-): It makes a ton of assumptions about its usage, and is NOT
// NOTE(qix-): suitable for use in any context other than the Oro kernel.
// NOTE(qix-):
// NOTE(qix-): DO NOT COPY THIS CODE INTO YOUR OWN PROJECTS IN ANY CAPACITY.
// NOTE(qix-): IT IS NOT SAFE. YOU HAVE BEEN WARNED.
// NOTE(qix-):
// NOTE(qix-): Similarly, if you are here to make edits to this code, please
// NOTE(qix-): be very careful and ensure that you are not introducing any
// NOTE(qix-): unsafety into the codebase. This is a very delicate module.
// NOTE(qix-): It will be HEAVILY scrutinized in code review. Be ready.

use core::{
	marker::PhantomData,
	mem::{size_of, ManuallyDrop, MaybeUninit},
	ops::Deref,
	sync::atomic::{AtomicUsize, Ordering},
};
use oro_macro::unlikely;
use oro_mem::{
	mapper::{AddressSegment, AddressSpace, MapError},
	pfa::alloc::Alloc,
	translate::Translator,
};
use oro_sync::spinlock::unfair_critical::{InterruptController, UnfairCriticalSpinlock};

/// A registry for reference-counted arena allocation.
///
/// The registry is a reference-counted arena allocator that
/// allows for the allocation of items that are reference-counted
/// across the system. The registry is designed to be used in
/// a supervisor space, and is not intended for use in user space.
///
/// Registry allocations return [`Handle`]s, which can be cloned
/// and will free the slot when the final user drops it.
pub(crate) struct Registry<T, IntCtrl, AddrSpace, Pat>
where
	T: Sized + 'static,
	IntCtrl: InterruptController,
	AddrSpace: AddressSpace,
	Pat: Translator,
{
	/// The base address of the registry.
	// TODO(qix-): Remove this field once const trait functions are stabilized,
	// TODO(qix-): replacing it with `segment.range().0 as *mut _` and saving
	// TODO(qix-): a few bytes.
	base: *mut MaybeUninit<ItemFrame<T>>,
	/// Bookkeeping counters used in the registry.
	bookkeeping: UnfairCriticalSpinlock<RegistryBookkeeping>,
	/// The segment this registry is in.
	segment: AddrSpace::SupervisorSegment,
	/// The mapper for the registry.
	mapper: AddrSpace::SupervisorHandle,
	/// The physical address translator (PAT) this registry will use.
	pat: Pat,
	/// The interrupt controller for the registry.
	_interrupt_controller: PhantomData<IntCtrl>,
	/// The address space for the registry.
	_address_space: PhantomData<AddrSpace>,
}

/// Registry-level bookkeeping fields, protected
/// behind an [`UnfairCriticalSpinlock`].
struct RegistryBookkeeping {
	/// The last free ID in the registry.
	///
	/// If this is `usize::MAX`, then there are no free slots.
	last_free_id:     usize,
	/// The total count of items in the registry.
	total_count:      usize,
	/// Total page count of the registry.
	total_page_count: usize,
}

impl RegistryBookkeeping {
	/// Creates a new instance of the registry bookkeeping.
	fn new() -> Self {
		Self {
			last_free_id:     usize::MAX,
			total_count:      0,
			total_page_count: 0,
		}
	}
}

/// A frame in the registry.
///
/// Wraps an item `T` with metadata about the slot itself,
/// used for bookkeeping purposes.
struct ItemFrame<T: Sized + 'static> {
	/// A union of the item or the next free index.
	maybe_item: MaybeItem<T>,
	/// Count of users of this item.
	/// In the event that this is zero, the item is free.
	/// In the event that this count reaches zero, the item gets dropped.
	user_count: AtomicUsize,
}

/// A union of either an occupied item slot, or the index of the
/// next free slot.
union MaybeItem<T: Sized + 'static> {
	/// The item itself.
	item:      ManuallyDrop<UnfairCriticalSpinlock<T>>,
	/// The next free index.
	next_free: usize,
}

impl<T, IntCtrl, AddrSpace, Pat> Registry<T, IntCtrl, AddrSpace, Pat>
where
	T: Sized + 'static,
	IntCtrl: InterruptController,
	AddrSpace: AddressSpace,
	Pat: Translator,
{
	/// Creates a new, empty registry in the given
	/// segment.
	///
	/// Makes the registry available for use across all
	/// cores in the system.
	///
	/// The segment used for the registry must be valid,
	/// unique to all other registries, and previously
	/// unpopulated (or this function will error with
	/// [`MapError::Exists`]).
	///
	/// Typically, this function should be called once
	/// at boot time.
	pub fn new<Pfa: Alloc>(
		pat: Pat,
		pfa: &mut Pfa,
		segment: AddrSpace::SupervisorSegment,
	) -> Result<Self, MapError> {
		// SAFETY(qix-): We can more or less guarantee that this registry
		// SAFETY(qix-): is being constructed in the supervisor space.
		// SAFETY(qix-): Further, we can't guarantee that the segment is
		// SAFETY(qix-): going to be accessed separately from other segments
		// SAFETY(qix-): quite yet, but we'll verify that we have exclusive
		// SAFETY(qix-): access to the segment directly after this call.
		let mapper = unsafe { AddrSpace::current_supervisor_space(&pat) };
		segment.provision_as_shared(&mapper, pfa, &pat)?;

		Ok(Self {
			base: segment.range().0 as *mut _,
			bookkeeping: UnfairCriticalSpinlock::new(RegistryBookkeeping::new()),
			pat,
			segment,
			mapper,
			_interrupt_controller: PhantomData,
			_address_space: PhantomData,
		})
	}

	/// Allocates and inserts an item `T` into the registry, permanently.
	/// Returns the `id` rather than a `Handle`. This is useful for
	/// items that are not intended to be reference-counted and must
	/// always be valid throughout the lifetime of the kernel.
	///
	/// Returns an error if there was a problem allocating the item.
	///
	/// Takes a reference to the spinlock itself, since not all allocations require
	/// locking the PFA.
	///
	/// # Safety
	/// Marked unsafe because misuse of this function can lead to
	/// memory leaks. You probably want to use [`Self::insert()`] instead.
	pub unsafe fn insert_permanent<Pfa: Alloc>(
		&self,
		pfa: &UnfairCriticalSpinlock<Pfa>,
		item: impl Into<T>,
	) -> Result<usize, MapError> {
		let item = item.into();

		// SAFETY(qix-): We don't panic in this function.
		let mut bk = unsafe { self.bookkeeping.lock::<IntCtrl>() };

		if bk.last_free_id == usize::MAX {
			let byte_offset = bk.total_count * size_of::<MaybeUninit<ItemFrame<T>>>();
			let byte_offset_end = byte_offset + size_of::<MaybeUninit<ItemFrame<T>>>();

			if unlikely!((self.segment.range().0 + byte_offset_end - 1) > self.segment.range().1) {
				return Err(MapError::VirtOutOfRange);
			}

			// TODO(qix-): If PFAs ever support more than 4K pages, this will need to be updated.
			let new_page_end = byte_offset_end & !4095;
			let new_page_count = new_page_end + 1;

			if new_page_count > bk.total_page_count {
				// SAFETY(qix-): We don't panic in this function.
				let mut pfa = unsafe { pfa.lock::<IntCtrl>() };

				for page_id in bk.total_page_count..new_page_count {
					let page = pfa.allocate().ok_or(MapError::OutOfMemory)?;

					// TODO(qix-): If PFAs ever support more than 4K pages, this will need to be updated.
					let virt = self.segment.range().0 + page_id * 4096;
					if let Err(err) =
						self.segment
							.map(&self.mapper, &mut *pfa, &self.pat, virt, page)
					{
						// SAFETY(qix-): We just allocated this page and the mapper didn't use it.
						unsafe {
							pfa.free(page);
						}
						return Err(err);
					}

					// Increment on each loop such that if we fail, a future attempt won't try to
					// re-map the same virtual addresses.
					bk.total_page_count += 1;
				}
			}

			let id = bk.total_count;
			bk.total_count += 1;

			let slot = unsafe { &mut *self.base.add(id) };
			slot.write(ItemFrame {
				maybe_item: MaybeItem {
					item: ManuallyDrop::new(UnfairCriticalSpinlock::new(item)),
				},
				user_count: AtomicUsize::new(1),
			});

			Ok(id)
		} else {
			let id = bk.last_free_id;
			let slot = unsafe { (*self.base.add(id)).assume_init_mut() };
			bk.last_free_id = unsafe { slot.maybe_item.next_free };
			let last_user_count = slot.user_count.fetch_add(1, Ordering::Relaxed);
			debug_assert_eq!(last_user_count, 0);
			slot.maybe_item.item = ManuallyDrop::new(UnfairCriticalSpinlock::new(item));

			Ok(id)
		}
	}

	/// Allocates and inserts an item `T` into the registry.
	///
	/// Returns an error if there was a problem allocating the item.
	///
	/// Takes a reference to the spinlock itself, since not all allocations require
	/// locking the PFA.
	pub fn insert<Pfa: Alloc>(
		&'static self,
		pfa: &UnfairCriticalSpinlock<Pfa>,
		item: impl Into<T>,
	) -> Result<Handle<T>, MapError> {
		// SAFETY(qix-): `insert_permanent` simply creates a new item
		// SAFETY(qix-): with a user count of 1, but doesn't return a handle
		// SAFETY(qix-): to it. Since this is the only other place that
		// SAFETY(qix-): a `Handle` can even be constructed, it means
		// SAFETY(qix-): all other usages really *are* permanent, but ours
		// SAFETY(qix-): is not and instead piggie-backs off the user count
		// SAFETY(qix-): being 1 to simply initialize a handle that *does*
		// SAFETY(qix-): become reference counted.
		let id = unsafe { self.insert_permanent(pfa, item)? };
		Ok(Handle { id, registry: self })
	}

	/// Returns the item at the given ID, or `None` if the ID is invalid.
	///
	/// **This function incurs a registry lock.**
	/// You should use [`Handle`]s wherever possible, which do not
	/// incur registry locks.
	///
	/// # Safety
	/// **DO NOT PERFORM LOOKUPS BY ID FOR ANYTHING SECURITY-RELATED.**
	///
	/// IDs are RE-USABLE and may not refer to the same item if the item
	/// slot is dropped and re-allocated.
	///
	/// For that reason, this function is marked as unsafe.
	pub unsafe fn get(&'static self, id: usize) -> Option<Handle<T>> {
		// We have to keep this lock open even during the lookup
		// since user counts are not locked at the record level
		// and there is no "fetch_and_increment_unless_zero" atomic
		// operation.
		//
		// NOTE(qix-): We could load and then do a compare-and-swap, but this function
		// NOTE(qix-): really should be seldom used, and I'm not interested in
		// NOTE(qix-): fleshing it out further at this time. PR welcome.
		let bk = self.bookkeeping.lock::<IntCtrl>();

		if id >= bk.total_count {
			return None;
		}

		let slot = (*self.base.add(id)).assume_init_ref();

		// NOTE(qix-): Here's the part that could be changed
		// NOTE(qix-): to a compare-and-swap.
		if slot.user_count.load(Ordering::Relaxed) == 0 {
			None
		} else {
			slot.user_count.fetch_add(1, Ordering::Relaxed);
			Some(Handle { id, registry: self })
		}
	}
}

/// Handles item access and dropping in the registry.
trait RegistryAccess<T: Sized + 'static> {
	/// Gets the item frame at the given ID.
	///
	/// # Safety
	/// Caller must ensure that the ID is valid.
	/// This function performs no bounds checks,
	/// and assumes if an ID is passed in, it is
	/// valid.
	unsafe fn get(&self, id: usize) -> &UnfairCriticalSpinlock<T>;

	/// Increments the user count of the item at the given ID.
	///
	/// # Safety
	/// Caller must ensure that the ID is valid.
	/// This function performs no bounds checks,
	/// and assumes if an ID is passed in, it is
	/// valid.
	///
	/// The caller must ensure that [`Self::forget_item_at()`]
	/// is called when the item is no longer needed.
	unsafe fn lease_item_at(&self, id: usize);

	/// Forgets the item at the given ID.
	///
	/// If this is the last user of the item, the item
	/// will be dropped.
	///
	/// # Safety
	/// Caller must ensure that the ID is valid.
	/// This function performs no bounds checks,
	/// and assumes if an ID is passed in, it is
	/// valid.
	///
	/// Any references or handles to the item
	/// must be dropped before calling this function.
	unsafe fn forget_item_at(&self, id: usize);
}

impl<T, IntCtrl, AddrSpace, Pat> RegistryAccess<T> for Registry<T, IntCtrl, AddrSpace, Pat>
where
	T: Sized + 'static,
	IntCtrl: InterruptController,
	AddrSpace: AddressSpace,
	Pat: Translator,
{
	unsafe fn get(&self, id: usize) -> &UnfairCriticalSpinlock<T> {
		&(*self.base.add(id)).assume_init_ref().maybe_item.item
	}

	unsafe fn lease_item_at(&self, id: usize) {
		let last_user_count = (*self.base.add(id))
			.assume_init_ref()
			.user_count
			.fetch_add(1, Ordering::Relaxed);
		debug_assert_eq!(last_user_count, 0);
	}

	unsafe fn forget_item_at(&self, id: usize) {
		let slot = &mut *self.base.add(id);

		let last_user_count = slot
			.assume_init_ref()
			.user_count
			.fetch_sub(1, Ordering::Relaxed);

		debug_assert_ne!(last_user_count, 0);

		if last_user_count == 1 {
			let slot = slot.assume_init_mut();

			ManuallyDrop::drop(&mut slot.maybe_item.item);

			// SAFETY(qix-): DO NOT PUT THIS LOCK BEFORE THE ABOVE DROP.
			// SAFETY(qix-): YOU WILL DEADLOCK THE KERNEL.
			let mut bk = self.bookkeeping.lock::<IntCtrl>();
			slot.maybe_item.next_free = bk.last_free_id;
			bk.last_free_id = id;
		}
	}
}

/// A lightweight handle to an item in a registry.
///
/// The handle is a reference-counted item in the registry,
/// and is a thin wrapper around an [`UnfairCriticalSpinlock`]
/// to the item itself.
///
/// Handles can be safely `clone()`d. When the last handle
/// is dropped, the item is freed from the registry, where
/// the backing memory is reused for future allocations.
#[must_use]
pub struct Handle<T: Sized + 'static> {
	/// The ID of the item in the registry.
	///
	/// This is the offset into the registry's base address.
	///
	/// **DO NOT USE THIS ID FOR ANYTHING SECURITY-SENSITIVE.**
	id:       usize,
	/// The registry the item is in.
	registry: &'static dyn RegistryAccess<T>,
}

impl<T: Sized + 'static> Handle<T> {
	/// Returns the ID of the item in the registry.
	///
	/// **DO NOT USE THIS ID FOR ANYTHING SECURITY-SENSITIVE.**
	/// You should use `Handle`s wherever possible.
	///
	/// Note that this ID may go stale if the item is
	/// dropped and re-allocated. Future lookups
	/// using the given ID **_MAY_ NOT** refer to the
	/// same item.
	///
	/// **Do not rely on this ID for anything other
	/// than debugging or logging purposes.**
	#[must_use]
	pub fn id(&self) -> usize {
		self.id
	}
}

impl<T: Sized + 'static> Deref for Handle<T> {
	type Target = UnfairCriticalSpinlock<T>;

	fn deref(&self) -> &Self::Target {
		// SAFETY(qix-): We can assume that, given this handle
		// SAFETY(qix-): is even created (and cannot be created
		// SAFETY(qix-): externally), the ID is valid.
		unsafe { self.registry.get(self.id) }
	}
}

impl<T: Sized + 'static> Clone for Handle<T> {
	fn clone(&self) -> Self {
		// SAFETY(qix-): We can assume that, given this handle
		// SAFETY(qix-): is even created (and cannot be created
		// SAFETY(qix-): externally), the ID is valid.
		unsafe {
			self.registry.lease_item_at(self.id);
		}

		Self {
			id:       self.id,
			registry: self.registry,
		}
	}
}

impl<T: Sized + 'static> Drop for Handle<T> {
	fn drop(&mut self) {
		// SAFETY(qix-): We can assume that, given this handle
		// SAFETY(qix-): is even created (and cannot be created
		// SAFETY(qix-): externally), the ID is valid.
		unsafe {
			self.registry.forget_item_at(self.id);
		}
	}
}

/// Doubly linked collection adapter interface for a registry.
///
/// Collections are used via two registries:
/// - The item registry, storing type `T` items.
/// - The list registry, storing [`Item<T>`] items.
///
/// Then other structures that want to store independent
/// lists of type `T` would use this structure to manage
/// the list.
///
/// # Not a List
/// This type is not a list, set, or any other convention
/// datatype, despite looking like one.
///
/// It is meant to group together handles of type `T` in
/// a manner that works for arenas (specifically,
/// registries).
///
/// An `Item` is doubly linked, and there is no concept
/// of a "beginning" or "end" of the list - at least,
/// not in any managed way. Having a handle to an `Item<T>`
/// is not a handle to a list, but a handle to a single
/// item in a list, and traversal can go in either direction
/// depending on if `prev` or `next` is used.
///
/// There is no way to get the "first" item in the list
/// without traversal, and there is no way to get the "last"
/// in any other way, either.
///
/// This means that item collections can be merged, split,
/// spliced, re-ordered, etc. quite freely at the expense
/// of not providing common list-like operations (which
/// generally are _not_ needed in the Oro kernel).
///
/// # Loops
/// Items cannot directly be linked to themselves via the
/// accessor methods; all methods only take handles to the
/// underlying items. While this does not prevent duplicates
/// in the list, it does prevent loops.
///
/// For this reason, almost all methods that work on
/// `Handle<Item<T>>` will mutate the underlying registry
/// in some way.
pub struct Item<T: Sized + 'static> {
	/// The previous item in the list, or `None` if there is no previous item.
	prev:   Option<Handle<Item<T>>>,
	/// The next item in the list, or `None` if there is no next item.
	next:   Option<Handle<Item<T>>>,
	/// The handle to the item in its respective registry.
	handle: Handle<T>,
}

impl<T: Sized + 'static> Item<T> {
	/// Creates a new item with the given handle.
	///
	/// The item is not linked to any other items.
	#[must_use]
	pub fn new(handle: Handle<T>) -> Self {
		Self {
			prev: None,
			next: None,
			handle,
		}
	}
}

impl<T: Sized + 'static> Deref for Item<T> {
	type Target = UnfairCriticalSpinlock<T>;

	fn deref(&self) -> &Self::Target {
		&self.handle
	}
}

// impl simply for `Registry<Item<T>, ...>`
impl<T: Sized + 'static, IntCtrl, AddrSpace, Pat> Registry<Item<T>, IntCtrl, AddrSpace, Pat>
where
	IntCtrl: InterruptController,
	AddrSpace: AddressSpace,
	Pat: Translator,
{
	/// Inserts an item after the given item.
	///
	/// If the given item already has a 'next'
	/// item, the new item will be inserted between
	/// the given item and its 'next' item.
	#[expect(dead_code)]
	pub fn insert_after<Pfa: Alloc>(
		&'static self,
		pfa: &UnfairCriticalSpinlock<Pfa>,
		item: &Handle<Item<T>>,
		new_item: impl Into<Item<T>>,
	) -> Result<Handle<Item<T>>, MapError> {
		let handle = self.insert(pfa, new_item)?;

		// SAFETY(qix-): We will not panic here.
		{
			let mut item_lock = unsafe { item.lock::<IntCtrl>() };
			let mut new_item_lock = unsafe { handle.lock::<IntCtrl>() };
			new_item_lock.prev = Some(item.clone());
			new_item_lock.next.clone_from(&item_lock.next);
			item_lock.next = Some(handle.clone());
		}

		Ok(handle)
	}

	/// Returns an iterator over linked items,
	/// starting at the given item and traversing
	/// forward.
	///
	/// Any previous items are ignored.
	#[allow(dead_code, clippy::unused_self)]
	pub fn iter_forward(
		&'static self,
		item: &Option<Handle<Item<T>>>,
	) -> impl Iterator<Item = Handle<Item<T>>> {
		ItemIter {
			current:   item.clone(),
			direction: ItemIterDirection::Forward,
			_int_ctrl: PhantomData::<IntCtrl>,
		}
	}

	/// Returns an iterator over linked items,
	/// starting at the given item and traversing
	/// backward.
	///
	/// Any next items are ignored.
	#[allow(dead_code, clippy::unused_self)]
	pub fn iter_backward(
		&'static self,
		item: &Option<Handle<Item<T>>>,
	) -> impl Iterator<Item = Handle<Item<T>>> {
		ItemIter {
			current:   item.clone(),
			direction: ItemIterDirection::Backward,
			_int_ctrl: PhantomData::<IntCtrl>,
		}
	}

	/// Returns an iterator over all items,
	/// starting at the first item in the list.
	///
	/// This function will immediately perform
	/// a reverse traversal to find the first
	/// item in the list, and then return
	/// an iterator starting at that item.
	#[allow(dead_code, clippy::unused_self)]
	pub fn iter_all(
		&'static self,
		item: &Option<Handle<Item<T>>>,
	) -> impl Iterator<Item = Handle<Item<T>>> {
		ItemIter {
			current:   self.iter_backward(item).last(),
			direction: ItemIterDirection::Forward,
			_int_ctrl: PhantomData::<IntCtrl>,
		}
	}
}

impl<T: Sized + 'static> From<Handle<T>> for Item<T> {
	fn from(handle: Handle<T>) -> Self {
		Self::new(handle)
	}
}

/// The direction of an item iterator.
enum ItemIterDirection {
	/// Forward iteration.
	Forward,
	/// Backward iteration.
	Backward,
}

/// An iterator over linked [`Item`]s in a registry.
pub struct ItemIter<T: Sized + 'static, IntCtrl: InterruptController> {
	/// The current item in the iteration.
	current:   Option<Handle<Item<T>>>,
	/// The direction of the iteration.
	direction: ItemIterDirection,
	/// The interrupt controller for the iteration.
	_int_ctrl: PhantomData<IntCtrl>,
}

impl<T: Sized + 'static, IntCtrl: InterruptController> Iterator for ItemIter<T, IntCtrl> {
	type Item = Handle<Item<T>>;

	fn next(&mut self) -> Option<Self::Item> {
		let current = self.current.take()?;

		let next = match self.direction {
			ItemIterDirection::Forward => unsafe { current.lock::<IntCtrl>().next.clone() },
			ItemIterDirection::Backward => unsafe { current.lock::<IntCtrl>().prev.clone() },
		};

		self.current.clone_from(&next);

		next
	}
}

/// Iterator adapter trait, implemented for `Handle<Item<T>>` and
/// `Option<Handle<Item<T>>>`.
pub trait ItemIterEx {
	/// The base, most inner type that is wrapped by handles and items.
	type Item: Sized + 'static;

	/// Creates a forward iterator over the items,
	/// ignoring any previous items.
	fn iter_forward<IntCtrl: InterruptController>(&self) -> ItemIter<Self::Item, IntCtrl>;

	/// Creates a backward iterator over the items,
	/// ignoring any next items.
	fn iter_backward<IntCtrl: InterruptController>(&self) -> ItemIter<Self::Item, IntCtrl>;

	/// Creates an iterator over all items,
	/// first traversing backward to find the first
	/// item in the list, and then returning a forward
	/// iterator starting at that item.
	fn iter_all<IntCtrl: InterruptController>(&self) -> ItemIter<Self::Item, IntCtrl> {
		ItemIter {
			current:   self.iter_backward::<IntCtrl>().last(),
			direction: ItemIterDirection::Forward,
			_int_ctrl: PhantomData,
		}
	}
}

impl<T: Sized + 'static> ItemIterEx for Handle<Item<T>> {
	type Item = T;

	fn iter_forward<IntCtrl: InterruptController>(&self) -> ItemIter<Self::Item, IntCtrl> {
		ItemIter {
			current:   Some(self.clone()),
			direction: ItemIterDirection::Forward,
			_int_ctrl: PhantomData,
		}
	}

	fn iter_backward<IntCtrl: InterruptController>(&self) -> ItemIter<Self::Item, IntCtrl> {
		ItemIter {
			current:   Some(self.clone()),
			direction: ItemIterDirection::Backward,
			_int_ctrl: PhantomData,
		}
	}
}

impl<T: Sized + 'static> ItemIterEx for Option<Handle<Item<T>>> {
	type Item = T;

	fn iter_forward<IntCtrl: InterruptController>(&self) -> ItemIter<Self::Item, IntCtrl> {
		ItemIter {
			current:   self.clone(),
			direction: ItemIterDirection::Forward,
			_int_ctrl: PhantomData,
		}
	}

	fn iter_backward<IntCtrl: InterruptController>(&self) -> ItemIter<Self::Item, IntCtrl> {
		ItemIter {
			current:   self.clone(),
			direction: ItemIterDirection::Backward,
			_int_ctrl: PhantomData,
		}
	}
}
