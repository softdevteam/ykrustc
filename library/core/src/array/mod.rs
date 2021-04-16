//! Implementations of things like `Eq` for fixed-length arrays
//! up to a certain length. Eventually, we should be able to generalize
//! to all lengths.
//!
//! *[See also the array primitive type](array).*

#![stable(feature = "core_array", since = "1.36.0")]

use crate::borrow::{Borrow, BorrowMut};
use crate::cmp::Ordering;
use crate::convert::{Infallible, TryFrom};
use crate::fmt;
use crate::hash::{self, Hash};
use crate::iter::TrustedLen;
use crate::mem::{self, MaybeUninit};
use crate::ops::{Index, IndexMut};
use crate::slice::{Iter, IterMut};

mod iter;

#[stable(feature = "array_value_iter", since = "1.51.0")]
pub use iter::IntoIter;

/// Converts a reference to `T` into a reference to an array of length 1 (without copying).
#[unstable(feature = "array_from_ref", issue = "77101")]
pub fn from_ref<T>(s: &T) -> &[T; 1] {
    // SAFETY: Converting `&T` to `&[T; 1]` is sound.
    unsafe { &*(s as *const T).cast::<[T; 1]>() }
}

/// Converts a mutable reference to `T` into a mutable reference to an array of length 1 (without copying).
#[unstable(feature = "array_from_ref", issue = "77101")]
pub fn from_mut<T>(s: &mut T) -> &mut [T; 1] {
    // SAFETY: Converting `&mut T` to `&mut [T; 1]` is sound.
    unsafe { &mut *(s as *mut T).cast::<[T; 1]>() }
}

/// The error type returned when a conversion from a slice to an array fails.
#[stable(feature = "try_from", since = "1.34.0")]
#[derive(Debug, Copy, Clone)]
pub struct TryFromSliceError(());

#[stable(feature = "core_array", since = "1.36.0")]
impl fmt::Display for TryFromSliceError {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.__description(), f)
    }
}

impl TryFromSliceError {
    #[unstable(
        feature = "array_error_internals",
        reason = "available through Error trait and this method should not \
                     be exposed publicly",
        issue = "none"
    )]
    #[inline]
    #[doc(hidden)]
    pub fn __description(&self) -> &str {
        "could not convert slice to array"
    }
}

#[stable(feature = "try_from_slice_error", since = "1.36.0")]
impl From<Infallible> for TryFromSliceError {
    fn from(x: Infallible) -> TryFromSliceError {
        match x {}
    }
}

#[stable(feature = "rust1", since = "1.0.0")]
impl<T, const N: usize> AsRef<[T]> for [T; N] {
    #[inline]
    fn as_ref(&self) -> &[T] {
        &self[..]
    }
}

#[stable(feature = "rust1", since = "1.0.0")]
impl<T, const N: usize> AsMut<[T]> for [T; N] {
    #[inline]
    fn as_mut(&mut self) -> &mut [T] {
        &mut self[..]
    }
}

#[stable(feature = "array_borrow", since = "1.4.0")]
impl<T, const N: usize> Borrow<[T]> for [T; N] {
    fn borrow(&self) -> &[T] {
        self
    }
}

#[stable(feature = "array_borrow", since = "1.4.0")]
impl<T, const N: usize> BorrowMut<[T]> for [T; N] {
    fn borrow_mut(&mut self) -> &mut [T] {
        self
    }
}

#[stable(feature = "try_from", since = "1.34.0")]
impl<T, const N: usize> TryFrom<&[T]> for [T; N]
where
    T: Copy,
{
    type Error = TryFromSliceError;

    fn try_from(slice: &[T]) -> Result<[T; N], TryFromSliceError> {
        <&Self>::try_from(slice).map(|r| *r)
    }
}

#[stable(feature = "try_from", since = "1.34.0")]
impl<'a, T, const N: usize> TryFrom<&'a [T]> for &'a [T; N] {
    type Error = TryFromSliceError;

    fn try_from(slice: &[T]) -> Result<&[T; N], TryFromSliceError> {
        if slice.len() == N {
            let ptr = slice.as_ptr() as *const [T; N];
            // SAFETY: ok because we just checked that the length fits
            unsafe { Ok(&*ptr) }
        } else {
            Err(TryFromSliceError(()))
        }
    }
}

#[stable(feature = "try_from", since = "1.34.0")]
impl<'a, T, const N: usize> TryFrom<&'a mut [T]> for &'a mut [T; N] {
    type Error = TryFromSliceError;

    fn try_from(slice: &mut [T]) -> Result<&mut [T; N], TryFromSliceError> {
        if slice.len() == N {
            let ptr = slice.as_mut_ptr() as *mut [T; N];
            // SAFETY: ok because we just checked that the length fits
            unsafe { Ok(&mut *ptr) }
        } else {
            Err(TryFromSliceError(()))
        }
    }
}

#[stable(feature = "rust1", since = "1.0.0")]
impl<T: Hash, const N: usize> Hash for [T; N] {
    fn hash<H: hash::Hasher>(&self, state: &mut H) {
        Hash::hash(&self[..], state)
    }
}

#[stable(feature = "rust1", since = "1.0.0")]
impl<T: fmt::Debug, const N: usize> fmt::Debug for [T; N] {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&&self[..], f)
    }
}

#[stable(feature = "rust1", since = "1.0.0")]
impl<'a, T, const N: usize> IntoIterator for &'a [T; N] {
    type Item = &'a T;
    type IntoIter = Iter<'a, T>;

    fn into_iter(self) -> Iter<'a, T> {
        self.iter()
    }
}

#[stable(feature = "rust1", since = "1.0.0")]
impl<'a, T, const N: usize> IntoIterator for &'a mut [T; N] {
    type Item = &'a mut T;
    type IntoIter = IterMut<'a, T>;

    fn into_iter(self) -> IterMut<'a, T> {
        self.iter_mut()
    }
}

#[stable(feature = "index_trait_on_arrays", since = "1.50.0")]
impl<T, I, const N: usize> Index<I> for [T; N]
where
    [T]: Index<I>,
{
    type Output = <[T] as Index<I>>::Output;

    #[inline]
    fn index(&self, index: I) -> &Self::Output {
        Index::index(self as &[T], index)
    }
}

#[stable(feature = "index_trait_on_arrays", since = "1.50.0")]
impl<T, I, const N: usize> IndexMut<I> for [T; N]
where
    [T]: IndexMut<I>,
{
    #[inline]
    fn index_mut(&mut self, index: I) -> &mut Self::Output {
        IndexMut::index_mut(self as &mut [T], index)
    }
}

#[stable(feature = "rust1", since = "1.0.0")]
impl<A, B, const N: usize> PartialEq<[B; N]> for [A; N]
where
    A: PartialEq<B>,
{
    #[inline]
    fn eq(&self, other: &[B; N]) -> bool {
        self[..] == other[..]
    }
    #[inline]
    fn ne(&self, other: &[B; N]) -> bool {
        self[..] != other[..]
    }
}

#[stable(feature = "rust1", since = "1.0.0")]
impl<A, B, const N: usize> PartialEq<[B]> for [A; N]
where
    A: PartialEq<B>,
{
    #[inline]
    fn eq(&self, other: &[B]) -> bool {
        self[..] == other[..]
    }
    #[inline]
    fn ne(&self, other: &[B]) -> bool {
        self[..] != other[..]
    }
}

#[stable(feature = "rust1", since = "1.0.0")]
impl<A, B, const N: usize> PartialEq<[A; N]> for [B]
where
    B: PartialEq<A>,
{
    #[inline]
    fn eq(&self, other: &[A; N]) -> bool {
        self[..] == other[..]
    }
    #[inline]
    fn ne(&self, other: &[A; N]) -> bool {
        self[..] != other[..]
    }
}

#[stable(feature = "rust1", since = "1.0.0")]
impl<A, B, const N: usize> PartialEq<&[B]> for [A; N]
where
    A: PartialEq<B>,
{
    #[inline]
    fn eq(&self, other: &&[B]) -> bool {
        self[..] == other[..]
    }
    #[inline]
    fn ne(&self, other: &&[B]) -> bool {
        self[..] != other[..]
    }
}

#[stable(feature = "rust1", since = "1.0.0")]
impl<A, B, const N: usize> PartialEq<[A; N]> for &[B]
where
    B: PartialEq<A>,
{
    #[inline]
    fn eq(&self, other: &[A; N]) -> bool {
        self[..] == other[..]
    }
    #[inline]
    fn ne(&self, other: &[A; N]) -> bool {
        self[..] != other[..]
    }
}

#[stable(feature = "rust1", since = "1.0.0")]
impl<A, B, const N: usize> PartialEq<&mut [B]> for [A; N]
where
    A: PartialEq<B>,
{
    #[inline]
    fn eq(&self, other: &&mut [B]) -> bool {
        self[..] == other[..]
    }
    #[inline]
    fn ne(&self, other: &&mut [B]) -> bool {
        self[..] != other[..]
    }
}

#[stable(feature = "rust1", since = "1.0.0")]
impl<A, B, const N: usize> PartialEq<[A; N]> for &mut [B]
where
    B: PartialEq<A>,
{
    #[inline]
    fn eq(&self, other: &[A; N]) -> bool {
        self[..] == other[..]
    }
    #[inline]
    fn ne(&self, other: &[A; N]) -> bool {
        self[..] != other[..]
    }
}

// NOTE: some less important impls are omitted to reduce code bloat
// __impl_slice_eq2! { [A; $N], &'b [B; $N] }
// __impl_slice_eq2! { [A; $N], &'b mut [B; $N] }

#[stable(feature = "rust1", since = "1.0.0")]
impl<T: Eq, const N: usize> Eq for [T; N] {}

#[stable(feature = "rust1", since = "1.0.0")]
impl<T: PartialOrd, const N: usize> PartialOrd for [T; N] {
    #[inline]
    fn partial_cmp(&self, other: &[T; N]) -> Option<Ordering> {
        PartialOrd::partial_cmp(&&self[..], &&other[..])
    }
    #[inline]
    fn lt(&self, other: &[T; N]) -> bool {
        PartialOrd::lt(&&self[..], &&other[..])
    }
    #[inline]
    fn le(&self, other: &[T; N]) -> bool {
        PartialOrd::le(&&self[..], &&other[..])
    }
    #[inline]
    fn ge(&self, other: &[T; N]) -> bool {
        PartialOrd::ge(&&self[..], &&other[..])
    }
    #[inline]
    fn gt(&self, other: &[T; N]) -> bool {
        PartialOrd::gt(&&self[..], &&other[..])
    }
}

/// Implements comparison of arrays [lexicographically](Ord#lexicographical-comparison).
#[stable(feature = "rust1", since = "1.0.0")]
impl<T: Ord, const N: usize> Ord for [T; N] {
    #[inline]
    fn cmp(&self, other: &[T; N]) -> Ordering {
        Ord::cmp(&&self[..], &&other[..])
    }
}

// The Default impls cannot be done with const generics because `[T; 0]` doesn't
// require Default to be implemented, and having different impl blocks for
// different numbers isn't supported yet.

macro_rules! array_impl_default {
    {$n:expr, $t:ident $($ts:ident)*} => {
        #[stable(since = "1.4.0", feature = "array_default")]
        impl<T> Default for [T; $n] where T: Default {
            fn default() -> [T; $n] {
                [$t::default(), $($ts::default()),*]
            }
        }
        array_impl_default!{($n - 1), $($ts)*}
    };
    {$n:expr,} => {
        #[stable(since = "1.4.0", feature = "array_default")]
        impl<T> Default for [T; $n] {
            fn default() -> [T; $n] { [] }
        }
    };
}

array_impl_default! {32, T T T T T T T T T T T T T T T T T T T T T T T T T T T T T T T T}

#[lang = "array"]
impl<T, const N: usize> [T; N] {
    /// Returns an array of the same size as `self`, with function `f` applied to each element
    /// in order.
    ///
    /// # Examples
    ///
    /// ```
    /// #![feature(array_map)]
    /// let x = [1, 2, 3];
    /// let y = x.map(|v| v + 1);
    /// assert_eq!(y, [2, 3, 4]);
    ///
    /// let x = [1, 2, 3];
    /// let mut temp = 0;
    /// let y = x.map(|v| { temp += 1; v * temp });
    /// assert_eq!(y, [1, 4, 9]);
    ///
    /// let x = ["Ferris", "Bueller's", "Day", "Off"];
    /// let y = x.map(|v| v.len());
    /// assert_eq!(y, [6, 9, 3, 3]);
    /// ```
    #[unstable(feature = "array_map", issue = "75243")]
    pub fn map<F, U>(self, f: F) -> [U; N]
    where
        F: FnMut(T) -> U,
    {
        // SAFETY: we know for certain that this iterator will yield exactly `N`
        // items.
        unsafe { collect_into_array_unchecked(&mut IntoIter::new(self).map(f)) }
    }

    /// 'Zips up' two arrays into a single array of pairs.
    ///
    /// `zip()` returns a new array where every element is a tuple where the
    /// first element comes from the first array, and the second element comes
    /// from the second array. In other words, it zips two arrays together,
    /// into a single one.
    ///
    /// # Examples
    ///
    /// ```
    /// #![feature(array_zip)]
    /// let x = [1, 2, 3];
    /// let y = [4, 5, 6];
    /// let z = x.zip(y);
    /// assert_eq!(z, [(1, 4), (2, 5), (3, 6)]);
    /// ```
    #[unstable(feature = "array_zip", issue = "80094")]
    pub fn zip<U>(self, rhs: [U; N]) -> [(T, U); N] {
        let mut iter = IntoIter::new(self).zip(IntoIter::new(rhs));

        // SAFETY: we know for certain that this iterator will yield exactly `N`
        // items.
        unsafe { collect_into_array_unchecked(&mut iter) }
    }

    /// Returns a slice containing the entire array. Equivalent to `&s[..]`.
    #[unstable(feature = "array_methods", issue = "76118")]
    pub fn as_slice(&self) -> &[T] {
        self
    }

    /// Returns a mutable slice containing the entire array. Equivalent to
    /// `&mut s[..]`.
    #[unstable(feature = "array_methods", issue = "76118")]
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        self
    }

    /// Borrows each element and returns an array of references with the same
    /// size as `self`.
    ///
    ///
    /// # Example
    ///
    /// ```
    /// #![feature(array_methods)]
    ///
    /// let floats = [3.1, 2.7, -1.0];
    /// let float_refs: [&f64; 3] = floats.each_ref();
    /// assert_eq!(float_refs, [&3.1, &2.7, &-1.0]);
    /// ```
    ///
    /// This method is particularly useful if combined with other methods, like
    /// [`map`](#method.map). This way, you can avoid moving the original
    /// array if its elements are not `Copy`.
    ///
    /// ```
    /// #![feature(array_methods, array_map)]
    ///
    /// let strings = ["Ferris".to_string(), "♥".to_string(), "Rust".to_string()];
    /// let is_ascii = strings.each_ref().map(|s| s.is_ascii());
    /// assert_eq!(is_ascii, [true, false, true]);
    ///
    /// // We can still access the original array: it has not been moved.
    /// assert_eq!(strings.len(), 3);
    /// ```
    #[unstable(feature = "array_methods", issue = "76118")]
    pub fn each_ref(&self) -> [&T; N] {
        // SAFETY: we know for certain that this iterator will yield exactly `N`
        // items.
        unsafe { collect_into_array_unchecked(&mut self.iter()) }
    }

    /// Borrows each element mutably and returns an array of mutable references
    /// with the same size as `self`.
    ///
    ///
    /// # Example
    ///
    /// ```
    /// #![feature(array_methods)]
    ///
    /// let mut floats = [3.1, 2.7, -1.0];
    /// let float_refs: [&mut f64; 3] = floats.each_mut();
    /// *float_refs[0] = 0.0;
    /// assert_eq!(float_refs, [&mut 0.0, &mut 2.7, &mut -1.0]);
    /// assert_eq!(floats, [0.0, 2.7, -1.0]);
    /// ```
    #[unstable(feature = "array_methods", issue = "76118")]
    pub fn each_mut(&mut self) -> [&mut T; N] {
        // SAFETY: we know for certain that this iterator will yield exactly `N`
        // items.
        unsafe { collect_into_array_unchecked(&mut self.iter_mut()) }
    }
}

/// Pulls `N` items from `iter` and returns them as an array. If the iterator
/// yields fewer than `N` items, this function exhibits undefined behavior.
///
/// See [`collect_into_array`] for more information.
///
///
/// # Safety
///
/// It is up to the caller to guarantee that `iter` yields at least `N` items.
/// Violating this condition causes undefined behavior.
unsafe fn collect_into_array_unchecked<I, const N: usize>(iter: &mut I) -> [I::Item; N]
where
    // Note: `TrustedLen` here is somewhat of an experiment. This is just an
    // internal function, so feel free to remove if this bound turns out to be a
    // bad idea. In that case, remember to also remove the lower bound
    // `debug_assert!` below!
    I: Iterator + TrustedLen,
{
    debug_assert!(N <= iter.size_hint().1.unwrap_or(usize::MAX));
    debug_assert!(N <= iter.size_hint().0);

    match collect_into_array(iter) {
        Some(array) => array,
        // SAFETY: covered by the function contract.
        None => unsafe { crate::hint::unreachable_unchecked() },
    }
}

/// Pulls `N` items from `iter` and returns them as an array. If the iterator
/// yields fewer than `N` items, `None` is returned and all already yielded
/// items are dropped.
///
/// Since the iterator is passed as a mutable reference and this function calls
/// `next` at most `N` times, the iterator can still be used afterwards to
/// retrieve the remaining items.
///
/// If `iter.next()` panicks, all items already yielded by the iterator are
/// dropped.
fn collect_into_array<I, const N: usize>(iter: &mut I) -> Option<[I::Item; N]>
where
    I: Iterator,
{
    if N == 0 {
        // SAFETY: An empty array is always inhabited and has no validity invariants.
        return unsafe { Some(mem::zeroed()) };
    }

    struct Guard<T, const N: usize> {
        ptr: *mut T,
        initialized: usize,
    }

    impl<T, const N: usize> Drop for Guard<T, N> {
        fn drop(&mut self) {
            debug_assert!(self.initialized <= N);

            let initialized_part = crate::ptr::slice_from_raw_parts_mut(self.ptr, self.initialized);

            // SAFETY: this raw slice will contain only initialized objects.
            unsafe {
                crate::ptr::drop_in_place(initialized_part);
            }
        }
    }

    let mut array = MaybeUninit::uninit_array::<N>();
    let mut guard: Guard<_, N> =
        Guard { ptr: MaybeUninit::slice_as_mut_ptr(&mut array), initialized: 0 };

    while let Some(item) = iter.next() {
        // SAFETY: `guard.initialized` starts at 0, is increased by one in the
        // loop and the loop is aborted once it reaches N (which is
        // `array.len()`).
        unsafe {
            array.get_unchecked_mut(guard.initialized).write(item);
        }
        guard.initialized += 1;

        // Check if the whole array was initialized.
        if guard.initialized == N {
            mem::forget(guard);

            // SAFETY: the condition above asserts that all elements are
            // initialized.
            let out = unsafe { MaybeUninit::array_assume_init(array) };
            return Some(out);
        }
    }

    // This is only reached if the iterator is exhausted before
    // `guard.initialized` reaches `N`. Also note that `guard` is dropped here,
    // dropping all already initialized elements.
    None
}
