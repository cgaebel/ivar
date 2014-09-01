//! A hole that can only be filled once, and taken once.
//!
//! This has no dependency on libstd, and has only 32-bits of overhead per ivar.
#![feature(phase)]
#![feature(unsafe_destructor)]
#![license = "MIT"]
#![no_std]
#![deny(missing_doc)]

extern crate alloc;
extern crate core;

#[cfg(test)] #[phase(plugin,link)] extern crate std;

#[cfg(test)] extern crate native;
#[cfg(test)] extern crate test;

use core::clone::Clone;
use core::kinds::marker;
use core::mem;
use core::ops::Drop;
use alloc::owned::Box;
use core::option::{Option,Some,None};
use core::ptr;
use core::ptr::RawPtr;

/// The actual ivar cell that ends up on the heap. Reference counting and the
/// "filled or not filled" bit are stored in the `meta`data field.
struct IVarCell<T> {
  data: T,
  // 1 bit - was this ivar ever filled?
  // 1 bit - is this ivar currently filled?
  // 30 bits - How many strong refs to this cell are there?
  meta: u32,
}

impl<T> IVarCell<T> {
  fn new() -> IVarCell<T> {
    unsafe {
      IVarCell {
        meta: 1u32, // start out with one strong ref.
        data: mem::uninitialized(),
      }
    }
  }

  #[inline(always)]
  fn was_ever_filled(&self) -> bool {
    (self.meta & 0x80000000u32) != 0
  }

  #[inline(always)]
  fn is_currently_filled(&self) -> bool {
    (self.meta & 0x40000000u32) != 0
  }

  #[inline(always)]
  fn mark_taken(&mut self) {
    self.meta &= !0x40000000u32;
  }

  #[inline(always)]
  fn set_filled(&mut self) {
    self.meta |= 0xC0000000u32;
  }

  #[inline(always)]
  fn strong_refs(&self) -> u32 {
    self.meta & !0x30000000u32
  }

  #[inline(always)]
  fn inc_ref(&mut self) {
    self.meta += 1;
  }

  /// Returns true iff the refcount is 0 after decrementing.
  #[inline(always)]
  fn dec_ref(&mut self) -> bool {
    self.meta -= 1;
    self.strong_refs() == 0
  }

  #[inline(always)]
  unsafe fn unsafe_read(&mut self) -> T {
    ptr::read(&self.data as *const T)
  }

  fn take(&mut self) -> Option<T> {
    unsafe {
      if self.is_currently_filled() {
        self.mark_taken();
        return Some(self.unsafe_read());
      }

      None
    }
  }

  #[inline(always)]
  fn peek(&self) -> Option<&T> {
    if self.is_currently_filled() {
      Some(&self.data)
    } else {
      None
    }
  }

  #[inline(always)]
  unsafe fn unsafe_write(&mut self, t: T) {
    let data_ptr: *mut T = mem::transmute(&self.data);
    ptr::write(data_ptr, t)
  }

  #[inline(always)]
  fn fill(&mut self, t: T) {
    unsafe {
      self.unsafe_write(t);
      self.set_filled();
    }
  }
}

#[unsafe_destructor]
impl<T> Drop for IVarCell<T> {
  fn drop(&mut self) {
    unsafe {
      if self.is_currently_filled() {
        self.unsafe_read();
      }
    }
  }
}

/// A handle to a heap-allocated ivar cell.
///
/// This is not to be exported to the public interface, since it allows both
/// reading and writing.
#[unsafe_no_drop_flag]
struct IVar<T> {
  cell: *mut IVarCell<T>,
  nosend: marker::NoSend,
  nosync: marker::NoSync,
}

impl<T> IVar<T> {
  fn new() -> IVar<T> {
    unsafe {
      let the_box: Box<IVarCell<T>> = box IVarCell::new();
      let as_ptr: *mut IVarCell<T> = mem::transmute(the_box);
      IVar {
        cell:   as_ptr,
        nosend: marker::NoSend,
        nosync: marker::NoSync,
      }
    }
  }

  fn make_ref(&self) -> IVar<T> {
    unsafe {
      (*self.cell).inc_ref();
      IVar {
        cell: self.cell,
        nosend: marker::NoSend,
        nosync: marker::NoSync,
      }
    }
  }
}

#[unsafe_destructor]
impl<T> Drop for IVar<T> {
  fn drop(&mut self) {
    if self.cell.is_null() { return; }

    unsafe {
      if (*self.cell).dec_ref() {
        let _: Box<IVarCell<T>> = mem::transmute(self.cell);
        self.cell = ptr::mut_null();
      }
    }
  }
}

/// A reading handle to an IVar.
///
/// This may be cloned so that multiple different places in your code may peek
/// at the IVar result. However, once any of the read handles `take`s it, no
/// other may. Horray for ownership semantics. If you want them to share,
/// consider using an `Rc<T>` and using `peek` instead of `take`.
pub struct IVarRd<T> {
  inner: IVar<T>,
}

impl<T> IVarRd<T> {
  /// Attempt to get a reference to the filled value. `None` is returned if the
  /// value has either not been filled, or already been taken.
  #[inline(always)]
  pub fn peek(&self) -> Option<&T> {
    unsafe { (*self.inner.cell).peek() }
  }

  /// Takes the value out of the IVar, emptying it. Subsequent `take`s and
  /// `peek`s will always return `None`.
  #[inline(always)]
  pub fn take(&mut self) -> Option<T> {
    unsafe { (*self.inner.cell).take() }
  }

  /// Does the IVar currently have a payload ready?
  #[inline(always)]
  pub fn is_filled(&self) -> bool {
    unsafe { (*self.inner.cell).is_currently_filled() }
  }

  /// Was the IVar ever filled at any point in time? Note that if it is
  /// currently not filled, but it was filled at some point in the past, it will
  /// never be filled again.
  #[inline(always)]
  pub fn was_ever_filled(&self) -> bool {
    unsafe { (*self.inner.cell).was_ever_filled() }
  }
}

impl<T> Clone for IVarRd<T> {
  fn clone(&self) -> IVarRd<T> {
    unsafe {
      (*self.inner.cell).inc_ref();
      IVarRd {
        inner: IVar {
          cell:   self.inner.cell,
          nosend: marker::NoSend,
          nosync: marker::NoSync,
        }
      }
    }
  }
}

/// A write handle to an IVar.
///
/// This may be used to fill the IVar with a value, once. The invariant of IVars
/// is that they may only be be filled once, and this is enforced at the type
/// system level by making `fill` take the write handle by-move. If you can find
/// a way to trigger a double-fill without an unsafe block, please file a bug.
pub struct IVarWr<T> {
  inner: IVar<T>,
}

impl<T> IVarWr<T> {
  /// Places the payload into the IVar, consuming the write handle.
  #[inline(always)]
  pub fn fill(self, t: T) {
    unsafe { (*self.inner.cell).fill(t) }
  }
}

/// Creates a new IVar, with a reading and writing handle.
pub fn new<T>() -> (IVarRd<T>, IVarWr<T>) {
  let iv_wr = IVar::new();
  let iv_rd = iv_wr.make_ref();
  (IVarRd { inner: iv_rd }, IVarWr { inner: iv_wr })
}

#[cfg(test)]
mod my_test {
  use super::new;
  use std::option::{None,Some};

  #[test]
  fn simple_usage() {
    let (mut rd, wr) = new();

    assert_eq!(rd.take(), None);
    wr.fill(1u);
    assert_eq!(rd.peek(), Some(&1u));
    assert_eq!(rd.take(), Some(1u));
    assert_eq!(rd.peek(), None);
  }
}
