use core::alloc::Layout;
use core::mem::{align_of, size_of};
use core::ptr::NonNull;
#[cfg(not(all(loom, test)))]
pub(crate) use core::sync::atomic::{AtomicUsize, Ordering};
#[cfg(all(loom, test))]
pub(crate) use loom::sync::atomic::{AtomicUsize, Ordering};

use alloc::borrow::Cow;
use alloc::boxed::Box;
use alloc::string::String;

/// A better atomically-reference counted string type.
///
/// # Benefits
///
/// - Only a single pointer. Great for cases where you want to keep the data
///   structure lightweight or need to do some FFI stuff with it.
///
/// - It's possible to create a const `arcstr` from a literal via the
///   [`literal_arcstr!`][crate::literal_arcstr] macro.
///
///   These are zero cost, take no heap allocation, and don't even need to
///   perform atomic reads/writes when being cloned or dropped (or at any other
///   time). They even get stored in the read-only memory of your executable,
///   which can be beneficial for performance and memory usage. (The downside is
///   that the API is a bit janky, see it's docs for why).
///
/// - [`ArcStr::new()`](ArcStr.html#method.new) is a `const` function. This
///   shouldn't be surprising given point 2 though. Naturally, this means that
///   `ArcStr::default()` is free (unlike `std::sync::Arc<str>::default()`).
///
/// - `ArcStr` is totally immutable. No need to lose sleep over code that thinks
///   it has a right to mutate your `Arc` just because it holds the only
///   reference.
///
/// - More implementations of various traits like `PartialEq<Other>` and such
///   that hopefully will help improve ergonomics.
///
/// - We don't support `Weak` references, which means the overhead of atomic
///   operations is lower. This one is also a drawback.
///
/// This offers performance benefits over `Arc<str>` or `Arc<String>` for some
/// use cases, and can be useful when working in the FFI. The crate's top-level
/// documentation has a number of compelling reasons to use this listed, so I
/// won't repeat them here.
///
/// # Usage
/// ## As a `str`
///
/// `ArcStr` implements `Deref<Target = str>`, and so all functions and methods
///  from `str` work on it, even though we don't expose them on `ArcStr`
///  directly. This is not unique to `ArcStr`, but is a frequent source of
///  confusion I've seen for types that implement `Deref`, for example:
///
/// ```
/// # use arcstr::ArcStr;
/// let s = ArcStr::from("something");
/// // These go through `Deref`, so they work even though
/// // there is no `ArcStr::len` or `ArcStr::eq_ignore_ascii_case` function
/// assert_eq!(s.len(), 9);
/// assert!(s.eq_ignore_ascii_case("SOMETHING"));
/// ```
///
/// Additionally, `&ArcStr` can be passed to any function which accepts `&str`.
/// For example:
///
/// ```
/// # use arcstr::ArcStr;
/// fn accepts_str(s: &str) {
///    # let _ = s;
///    // s...
/// }
///
/// let test_str: ArcStr = "test".into();
/// // This works even though `&test_str` is normally an `&ArcStr`
/// accepts_str(&test_str);
///
/// // Of course, this works for functionality from the standard library as well.
/// let test_but_loud = ArcStr::from("TEST");
/// assert!(test_str.eq_ignore_ascii_case(&test_but_loud));
/// ```
///
/// ## As a `const`
///
/// The big unique feature of `ArcStr`, aside from its charming personality, is
/// the ability to create static/const `ArcStr`s. This is kind of annoying,
/// since it requires unsafe, but the safety requirement is just UTF-8 validity
/// of the provided byte string. (See [the macro](crate::literal_arcstr) docs
/// for details.
///
/// ```
/// # use arcstr::{ArcStr, literal_arcstr};
/// const WOW: ArcStr = unsafe { literal_arcstr!(b"cool robot!") };
/// assert_eq!(WOW, "cool robot!");
/// ```
#[repr(transparent)]
pub struct ArcStr(NonNull<ThinInner>);

unsafe impl Sync for ArcStr {}
unsafe impl Send for ArcStr {}

impl ArcStr {
    /// Construct a new empty string.
    ///
    /// # Examples
    ///
    /// ```
    /// # use arcstr::ArcStr;
    /// let s = ArcStr::new();
    /// assert_eq!(s, "");
    /// ```
    #[inline]
    pub const fn new() -> Self {
        EMPTY
    }

    /// Extract a string slice containing our data.
    ///
    /// Note: This is an equivalent to our `Deref` implementation, but can be
    /// more readable than `&*s` in the cases where a manual invocation of
    /// `Deref` would be required.
    ///
    /// # Examples
    // TODO: find a better example where `&*` would have been required.
    /// ```
    /// # use arcstr::ArcStr;
    /// let s = ArcStr::from("abc");
    /// assert_eq!(s.as_str(), "abc");
    /// ```
    #[inline]
    pub fn as_str(&self) -> &str {
        self
    }

    /// Returns the length of this `ArcStr` in bytes.
    ///
    /// # Examples
    ///
    /// ```
    /// # use arcstr::ArcStr;
    /// let a = ArcStr::from("foo");
    /// assert_eq!(a.len(), 3);
    /// ```
    #[inline]
    pub fn len(&self) -> usize {
        unsafe { ThinInner::get_len_flags(self.0.as_ptr()).len() }
    }

    /// Returns true if this `ArcStr` is empty.
    ///
    /// # Examples
    ///
    /// ```
    /// # use arcstr::ArcStr;
    /// assert!(!ArcStr::from("foo").is_empty());
    /// assert!(ArcStr::new().is_empty());
    /// ```
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Convert us to a `std::string::String`.
    ///
    /// This is provided as an inherent method to avoid needing to route through
    /// the `Display` machinery, but is equivalent to `ToString::to_string`.
    ///
    /// # Examples
    ///
    /// ```
    /// # use arcstr::ArcStr;
    /// let s = ArcStr::from("abc");
    /// assert_eq!(s.to_string(), "abc");
    /// ```
    #[inline]
    pub fn to_string(&self) -> String {
        #[cfg(not(feature = "std"))]
        use alloc::borrow::ToOwned;
        self.as_str().to_owned()
    }

    /// Extract a byte slice containing the string's data.
    ///
    /// # Examples
    ///
    /// ```
    /// # use arcstr::ArcStr;
    /// let foobar = ArcStr::from("foobar");
    /// assert_eq!(foobar.as_bytes(), b"foobar");
    /// ```
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        let p = self.0.as_ptr();
        unsafe {
            let len = ThinInner::get_len_flags(p).len();
            let data = (p as *const u8).add(memoffset::offset_of!(ThinInner, data));
            debug_assert_eq!(&(*p).data as *const [u8; 0] as usize, data as usize);
            core::slice::from_raw_parts(data, len)
        }
    }

    /// Return the raw pointer this `ArcStr` wraps, for advanced use cases.
    ///
    /// Note that in addition to the `NonNull` constraint expressed in the type
    /// signature, we also guarantee the pointer has an alignment of at least 8
    /// bytes, even on platforms where a lower alignment would be acceptable.
    ///
    /// # Examples
    ///
    /// ```
    /// # use arcstr::ArcStr;
    /// let s = ArcStr::from("abcd");
    /// let p = ArcStr::into_raw(s);
    /// // Some time later...
    /// let s = unsafe { ArcStr::from_raw(p) };
    /// assert_eq!(s, "abcd");
    /// ```
    #[inline]
    pub fn into_raw(this: Self) -> NonNull<()> {
        let p = this.0;
        core::mem::forget(this);
        p.cast()
    }

    /// The opposite version of [`Self::into_raw`]. Still intended only for
    /// advanced use cases.
    ///
    /// # Safety
    ///
    /// This function must be used on a valid pointer returned from
    /// [`ArcStr::into_raw`]. Additionally, you must ensure that a given `ArcStr`
    /// instance is only dropped once.
    ///
    /// # Examples
    ///
    /// ```
    /// # use arcstr::ArcStr;
    /// let s = ArcStr::from("abcd");
    /// let p = ArcStr::into_raw(s);
    /// // Some time later...
    /// let s = unsafe { ArcStr::from_raw(p) };
    /// assert_eq!(s, "abcd");
    /// ```
    #[inline]
    pub unsafe fn from_raw(ptr: NonNull<()>) -> Self {
        Self(ptr.cast())
    }

    /// Returns true if the two `ArcStr`s point to the same allocation.
    ///
    /// Note that functions like `PartialEq` check this already, so there's
    /// no performance benefit to doing something like `ArcStr::ptr_eq(&a1, &a2) || (a1 == a2)`.
    ///
    /// Caveat: `const`s aren't guaranteed to only occur in an executable a
    /// single time, and so this may be non-deterministic for `ArcStr` defined
    /// in a `const` with [`literal_arcstr!`][crate::literal_arcstr], unless one
    /// was created by a `clone()` on the other.
    ///
    /// # Examples
    ///
    /// ```
    /// use arcstr::ArcStr;
    ///
    /// let foobar = ArcStr::from("foobar");
    /// let same_foobar = foobar.clone();
    /// let other_foobar = ArcStr::from("foobar");
    /// assert!(ArcStr::ptr_eq(&foobar, &same_foobar));
    /// assert!(!ArcStr::ptr_eq(&foobar, &other_foobar));
    ///
    /// const YET_AGAIN_A_DIFFERENT_FOOBAR: ArcStr = unsafe { arcstr::literal_arcstr!(b"foobar") };
    /// let strange_new_foobar = YET_AGAIN_A_DIFFERENT_FOOBAR.clone();
    /// let wild_blue_foobar = strange_new_foobar.clone();
    /// assert!(ArcStr::ptr_eq(&strange_new_foobar, &wild_blue_foobar));
    /// ```
    #[inline]
    pub fn ptr_eq(lhs: &Self, rhs: &Self) -> bool {
        core::ptr::eq(lhs.0.as_ptr(), rhs.0.as_ptr())
    }

    /// Returns the number of references that exist to this `ArcStr`. If this is
    /// a static `ArcStr` (For example, one from
    /// [`literal_arcstr!`][crate::literal_arcstr]), returns `None`.
    ///
    /// Despite the difference in return type, this is named to match the method
    /// from the stdlib's Arc:
    /// [`Arc::strong_count`][alloc::sync::Arc::strong_count].
    ///
    /// If you aren't sure how to handle static `ArcStr` in the context of this
    /// return value, `ArcStr::strong_count(&s).unwrap_or(usize::MAX)` is
    /// frequently reasonable.
    ///
    /// # Safety
    ///
    /// This method by itself is safe, but using it correctly requires extra
    /// care. Another thread can change the strong count at any time, including
    /// potentially between calling this method and acting on the result.
    ///
    /// However, it may never change from `None` to `Some` or from `Some` to
    /// `None` for a given `ArcStr` — whether or not it is static is determined
    /// at construction, and never changes.
    ///
    /// # Examples
    ///
    /// ### Dynamic ArcStr
    /// ```
    /// # use arcstr::ArcStr;
    /// let foobar = ArcStr::from("foobar");
    /// assert_eq!(Some(1), ArcStr::strong_count(&foobar));
    /// let also_foobar = ArcStr::clone(&foobar);
    /// assert_eq!(Some(2), ArcStr::strong_count(&foobar));
    /// assert_eq!(Some(2), ArcStr::strong_count(&also_foobar));
    /// ```
    ///
    /// ### Static ArcStr
    /// ```
    /// # use arcstr::{ArcStr, literal_arcstr};
    /// // Safety: This is safe because it consists of valid UTF-8.
    /// let baz = unsafe { literal_arcstr!(b"baz") };
    /// assert_eq!(None, ArcStr::strong_count(&baz));
    /// // Similarly:
    /// assert_eq!(None, ArcStr::strong_count(&ArcStr::default()));
    /// ```
    #[inline]
    pub fn strong_count(this: &Self) -> Option<usize> {
        let this = this.0.as_ptr();
        if unsafe { ThinInner::get_len_flags(this).is_static() } {
            None
        } else {
            unsafe { Some((*this).strong.load(Ordering::SeqCst)) }
        }
    }

    /// Returns true if `this` is a "static" ArcStr. For example, if it was
    /// created from a call to [`literal_arcstr!`][crate::literal_arcstr]),
    /// returned by `ArcStr::new`, etc.
    ///
    /// Static `ArcStr`s can be converted to `&'static str` for free using
    /// [`ArcStr::as_static`], without leaking memory — they're static constants
    /// in the program (somwhere).
    ///
    /// # Examples
    ///
    /// ```
    /// # use arcstr::ArcStr;
    /// const STATIC: ArcStr = unsafe { arcstr::literal_arcstr!(b"Electricity!") };
    /// assert!(ArcStr::is_static(&STATIC));
    ///
    /// let still_static = unsafe { arcstr::literal_arcstr!(b"Shocking!") };
    /// assert!(ArcStr::is_static(&still_static));
    /// assert!(ArcStr::is_static(&still_static.clone()), "Cloned statics are still static");
    ///
    /// let nonstatic = ArcStr::from("Grounded...");
    /// assert!(!ArcStr::is_static(&nonstatic));
    /// ```
    #[inline]
    pub fn is_static(this: &Self) -> bool {
        unsafe { ThinInner::get_len_flags(this.0.as_ptr()).is_static() }
    }

    /// Returns true if `this` is a "static" ArcStr. For example, if it was
    /// created from a call to [`literal_arcstr!`][crate::literal_arcstr]),
    /// returned by `ArcStr::new`, etc.
    ///
    /// Static `ArcStr`s can be converted to `&'static str` for free using
    /// [`ArcStr::as_static`], without leaking memory — they're static constants
    /// in the program (somwhere).
    ///
    /// # Examples
    ///
    /// ```
    /// # use arcstr::ArcStr;
    /// const STATIC: ArcStr = unsafe { arcstr::literal_arcstr!(b"Electricity!") };
    /// assert_eq!(ArcStr::as_static(&STATIC), Some("Electricity!"));
    ///
    /// // Note that they don't have to be consts, just made using `literal_arcstr!`:
    /// let still_static = unsafe { arcstr::literal_arcstr!(b"Shocking!") };
    /// assert_eq!(ArcStr::as_static(&still_static), Some("Shocking!"));
    /// // Cloning a static still produces a static.
    /// assert_eq!(ArcStr::as_static(&still_static.clone()), Some("Shocking!"));
    ///
    /// // But it won't work for strings from other sources.
    /// let nonstatic = ArcStr::from("Grounded...");
    /// assert_eq!(ArcStr::as_static(&nonstatic), None);
    /// ```
    #[inline]
    pub fn as_static(this: &Self) -> Option<&'static str> {
        if unsafe { ThinInner::get_len_flags(this.0.as_ptr()).is_static() } {
            // We know static strings live forever, so they can have a static lifetime.
            Some(unsafe { &*(this.as_str() as *const str) })
        } else {
            None
        }
    }

    // Not public API. Exists so the literal_arcstr macro can call it.
    #[inline]
    #[doc(hidden)]
    pub const unsafe fn new_static<B>(ptr: &'static StaticArcStrInner<B>) -> Self {
        Self(NonNull::new_unchecked(ptr as *const _ as *mut ThinInner))
    }
}

impl Clone for ArcStr {
    #[inline]
    fn clone(&self) -> Self {
        let this = self.0.as_ptr();
        unsafe {
            // debug_assert_eq!(memoffset::offset_of!(ThinInner, nonstatic), 0);
            // let nonstatic_p = this as *const _ as *const bool;
            let is_static = ThinInner::get_len_flags(this).is_static();
            if !is_static {
                // From libstd's impl:
                //
                // > Using a relaxed ordering is alright here, as knowledge of the
                // > original reference prevents other threads from erroneously deleting
                // > the object.
                //
                // See: https://doc.rust-lang.org/src/alloc/sync.rs.html#1073
                let n = (*this).strong.fetch_add(1, Ordering::Relaxed);
                // Protect against aggressive leaking of Arcs causing us to overflow `strong`.
                if n > (isize::MAX as usize) {
                    abort();
                }
            }
        }
        Self(self.0)
    }
}

impl Drop for ArcStr {
    #[inline]
    fn drop(&mut self) {
        let this = self.0.as_ptr();
        unsafe {
            if ThinInner::get_len_flags(this).is_static() {
                return;
            }
            if (*this).strong.fetch_sub(1, Ordering::Release) == 1 {
                // `libstd` uses a full acquire fence here but notes that it's
                // possibly overkill. `triomphe`/`servo_arc` some of firefox ref
                // counting uses a load like this.
                //
                // These are morally equivalent for this case, the fence being a
                // bit more obvious and the load having slightly better perf in
                // some theoretical scenarios... but for our use case both seem
                // unnecessary.
                //
                // The intention behind these is to synchronize with `Release`
                // writes to `strong` that are happening on other threads. That
                // is, after the load (or fence), writes (any write, but
                // specifically writes to any part of `this` are what we care
                // about) from other threads which happened before the latest
                // `Release` write to strong will become visible on this thread.
                //
                // The reason this feels unnecessary is that our data is
                // entirely immutable outside `(*this).strong`. There are no
                // writes we could possibly be interested in.
                //
                // That said, I'll keep (the cheaper variant of) it for now for
                // easier auditing and such... an because I'm not 100% sure that
                // changing the ordering here wouldn't require changing it for
                // the fetch_sub above, or the fetch_add in `clone`...
                let _ = (*this).strong.load(Ordering::Acquire);
                ThinInner::destroy_cold(this)
            }
        }
    }
}
// Caveat on the `static`/`strong` fields: "is_static" indicates if we're
// located in static data (as with empty string). is_static being false meanse
// we are a normal arc-ed string.
//
// While `ArcStr` claims to hold a pointer to a `ThinInner`, for the static case
// we actually are using a pointer to a `ThinInnerStatic`. These are the same
// except for the type of the refernce count field. The issue is: We kind of
// need the static ones to not have any interior mutability, so that `const`s
// can use them, and so that they may be stored in read-only memory.
//
// We do this by keeping a flag in `len_flags` flag to indicate which case we're
// in, and maintaining the invariant that if we're a `ThinInnerStatic` **we may
// never access `.strong` in any way**.
//
// This is more subtle than you might think, sinc AFAIK we're not legally
// allowed to create an `&InnerRepr<AtomicUsize>` until we're 100% sure it's
// nonstatic, and prior to determining it, we are forced to work from entirely
// behind a raw pointer...
#[repr(C, align(8))]
struct InnerRepr<RcTy> {
    len_flags: LenFlags,
    // kind of a misnomer since there are no weak refs rn.
    strong: RcTy,
    // #[cfg(debug_assertions)]
    // orig_layout: Layout,
    data: [u8; 0],
}

// Not public API, exists for macros. Separate only to keep InnerRepr less
// generic and minimize the number of things bits I need to expose in
// `$crate::private_::`.
//
// TODO: `ThinInnerStatic` is redundant w/ `StaticArcStrInner<[u8; 0]>` and
// should be removed/replaced.
#[repr(C, align(8))]
#[doc(hidden)]
pub struct StaticArcStrInner<Buf> {
    pub len_flags: usize,
    pub count: usize,
    pub data: Buf,
}

#[derive(Clone, Copy)]
#[repr(transparent)]
struct LenFlags(usize);

impl LenFlags {
    const EMPTY_STATIC: LenFlags = LenFlags(0);
    #[inline]
    const fn len(self) -> usize {
        self.0 >> 1
    }
    #[inline]
    const fn is_static(self) -> bool {
        (self.0 & 1) == 0
    }

    #[inline]
    fn from_len_static(l: usize, is_static: bool) -> Option<Self> {
        l.checked_mul(2).map(|l| Self(l | (!is_static as usize)))
    }
    #[inline]
    const fn from_len_static_raw(l: usize, is_static: bool) -> Self {
        Self(l << 1 | (!is_static as usize))
    }
}

type ThinInner = InnerRepr<AtomicUsize>;
type ThinInnerStatic = InnerRepr<usize>;
const _: [(); size_of::<ThinInnerStatic>()] = [(); size_of::<ThinInner>()];
const _: [(); align_of::<ThinInnerStatic>()] = [(); align_of::<ThinInner>()];

const EMPTY_INNER: &ThinInnerStatic = &ThinInnerStatic {
    len_flags: LenFlags::EMPTY_STATIC,
    strong: 0usize,
    // This is removed because it seems dodgy with the macro, and `miri` seems
    // to be able to catch mismatches anyway.
    // #[cfg(debug_assertions)]
    // orig_layout: Layout::new::<ThinInnerStatic>(),
    data: [],
};

const EMPTY: ArcStr =
    ArcStr(unsafe { NonNull::new_unchecked(EMPTY_INNER as *const _ as *mut ThinInner) });

impl ThinInner {
    fn allocate(data: &str) -> NonNull<Self> {
        const ALIGN: usize = align_of::<ThinInner>();

        let num_bytes = data.len();
        debug_assert_ne!(num_bytes, 0);

        let mo = memoffset::offset_of!(ThinInner, data);
        if num_bytes >= (isize::MAX as usize) - (mo + ALIGN) {
            alloc_overflow();
        }

        unsafe {
            debug_assert!(Layout::from_size_align(num_bytes + mo, ALIGN).is_ok());
            let layout = Layout::from_size_align_unchecked(num_bytes + mo, ALIGN);

            let alloced = alloc::alloc::alloc(layout);
            if alloced.is_null() {
                alloc::alloc::handle_alloc_error(layout);
            }

            let ptr = alloced as *mut ThinInner;

            // we actually already checked this above...
            debug_assert!(LenFlags::from_len_static(num_bytes, false).is_some());
            let lf = LenFlags::from_len_static_raw(num_bytes, false);
            debug_assert_eq!(lf.len(), num_bytes);
            debug_assert_eq!(lf.is_static(), false);

            core::ptr::write(&mut (*ptr).len_flags, lf);
            core::ptr::write(&mut (*ptr).strong, AtomicUsize::new(1));

            // #[cfg(debug_assertions)]
            // {
            //     core::ptr::write(&mut (*ptr).orig_layout, layout);
            // }
            debug_assert_eq!(
                (alloced as *const u8).wrapping_add(mo),
                (*ptr).data.as_ptr(),
            );
            debug_assert_eq!(&(*ptr).data as *const _ as *const u8, (*ptr).data.as_ptr());

            core::ptr::copy_nonoverlapping(data.as_ptr(), alloced.add(mo), num_bytes);

            NonNull::new_unchecked(ptr)
        }
    }
    #[inline]
    unsafe fn get_len_flags(p: *const ThinInner) -> LenFlags {
        debug_assert_eq!(memoffset::offset_of!(ThinInner, len_flags), 0);
        *p.cast()
    }

    #[cold]
    unsafe fn destroy_cold(p: *mut ThinInner) {
        let lf = Self::get_len_flags(p);
        debug_assert!(!lf.is_static());
        // debug_assert!((*p).nonstatic);
        let len = lf.len();
        let layout = {
            let size = len + memoffset::offset_of!(ThinInner, data);
            let align = align_of::<ThinInner>();
            // debug_assert_eq!(Layout::from_size_align(size, align), Ok((*p).orig_layout));
            Layout::from_size_align_unchecked(size, align)
        };
        alloc::alloc::dealloc(p as *mut _, layout);
    }
}

#[inline(never)]
#[cold]
fn alloc_overflow() -> ! {
    panic!("overflow during Layout computation")
}

impl From<&str> for ArcStr {
    #[inline]
    fn from(s: &str) -> Self {
        if s.is_empty() {
            Self::new()
        } else {
            Self(ThinInner::allocate(s))
        }
    }
}

impl core::ops::Deref for ArcStr {
    type Target = str;
    #[inline]
    fn deref(&self) -> &str {
        unsafe { core::str::from_utf8_unchecked(self.as_bytes()) }
    }
}

impl Default for ArcStr {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl From<String> for ArcStr {
    #[inline]
    fn from(v: String) -> Self {
        v.as_str().into()
    }
}

impl From<&mut str> for ArcStr {
    #[inline]
    fn from(s: &mut str) -> Self {
        let s: &str = s;
        Self::from(s)
    }
}

impl From<Box<str>> for ArcStr {
    #[inline]
    fn from(s: Box<str>) -> Self {
        Self::from(&s[..])
    }
}
impl From<ArcStr> for Box<str> {
    #[inline]
    fn from(s: ArcStr) -> Self {
        s.as_str().into()
    }
}
impl From<ArcStr> for alloc::rc::Rc<str> {
    #[inline]
    fn from(s: ArcStr) -> Self {
        s.as_str().into()
    }
}
impl From<ArcStr> for alloc::sync::Arc<str> {
    #[inline]
    fn from(s: ArcStr) -> Self {
        s.as_str().into()
    }
}
impl From<alloc::rc::Rc<str>> for ArcStr {
    #[inline]
    fn from(s: alloc::rc::Rc<str>) -> Self {
        let s: &str = &*s;
        Self::from(s)
    }
}
impl From<alloc::sync::Arc<str>> for ArcStr {
    #[inline]
    fn from(s: alloc::sync::Arc<str>) -> Self {
        let s: &str = &*s;
        Self::from(s)
    }
}
impl<'a> From<Cow<'a, str>> for ArcStr {
    #[inline]
    fn from(s: Cow<'a, str>) -> Self {
        let s: &str = &*s;
        Self::from(s)
    }
}
impl<'a> From<&'a ArcStr> for Cow<'a, str> {
    #[inline]
    fn from(s: &'a ArcStr) -> Self {
        Cow::Borrowed(s)
    }
}

impl<'a> From<ArcStr> for Cow<'a, str> {
    #[inline]
    fn from(s: ArcStr) -> Self {
        if let Some(st) = ArcStr::as_static(&s) {
            Cow::Borrowed(st)
        } else {
            Cow::Owned(s.to_string())
        }
    }
}

impl From<&String> for ArcStr {
    #[inline]
    fn from(s: &String) -> Self {
        Self::from(s.as_str())
    }
}
impl From<&ArcStr> for ArcStr {
    #[inline]
    fn from(s: &ArcStr) -> Self {
        s.clone()
    }
}

impl core::fmt::Debug for ArcStr {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(self.as_str(), f)
    }
}

impl core::fmt::Display for ArcStr {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(self.as_str(), f)
    }
}

impl PartialEq for ArcStr {
    #[inline]
    fn eq(&self, o: &Self) -> bool {
        ArcStr::ptr_eq(self, o) || PartialEq::eq(self.as_str(), o.as_str())
    }
    #[inline]
    fn ne(&self, o: &Self) -> bool {
        !ArcStr::ptr_eq(self, o) && PartialEq::ne(self.as_str(), o.as_str())
    }
}

impl Eq for ArcStr {}

macro_rules! impl_peq {
    (@one $a:ty, $b:ty) => {
        impl<'a> PartialEq<$b> for $a {
            #[inline]
            fn eq(&self, s: &$b) -> bool {
                PartialEq::eq(&self[..], &s[..])
            }
            #[inline]
            fn ne(&self, s: &$b) -> bool {
                PartialEq::ne(&self[..], &s[..])
            }
        }
    };
    ($(($a:ty, $b:ty),)+) => {$(
        impl_peq!(@one $a, $b);
        impl_peq!(@one $b, $a);
    )+};
}

impl_peq! {
    (ArcStr, str),
    (ArcStr, &'a str),
    (ArcStr, String),
    (ArcStr, Cow<'a, str>),
    (ArcStr, Box<str>),
    (ArcStr, alloc::sync::Arc<str>),
    (ArcStr, alloc::rc::Rc<str>),
}

impl PartialOrd for ArcStr {
    #[inline]
    fn partial_cmp(&self, s: &Self) -> Option<core::cmp::Ordering> {
        Some(self.as_str().cmp(s.as_str()))
    }
}

impl Ord for ArcStr {
    #[inline]
    fn cmp(&self, s: &Self) -> core::cmp::Ordering {
        self.as_str().cmp(s.as_str())
    }
}

impl core::hash::Hash for ArcStr {
    #[inline]
    fn hash<H: core::hash::Hasher>(&self, h: &mut H) {
        self.as_str().hash(h)
    }
}

macro_rules! impl_index {
    ($($IdxT:ty,)*) => {$(
        impl core::ops::Index<$IdxT> for ArcStr {
            type Output = str;
            #[inline]
            fn index(&self, i: $IdxT) -> &Self::Output {
                &self.as_str()[i]
            }
        }
    )*};
}

impl_index! {
    core::ops::RangeFull,
    core::ops::Range<usize>,
    core::ops::RangeFrom<usize>,
    core::ops::RangeTo<usize>,
    core::ops::RangeInclusive<usize>,
    core::ops::RangeToInclusive<usize>,
}

impl AsRef<str> for ArcStr {
    #[inline]
    fn as_ref(&self) -> &str {
        self
    }
}

impl AsRef<[u8]> for ArcStr {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl core::borrow::Borrow<str> for ArcStr {
    #[inline]
    fn borrow(&self) -> &str {
        self
    }
}

impl core::str::FromStr for ArcStr {
    type Err = core::convert::Infallible;
    #[inline]
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::from(s))
    }
}

#[cold]
#[inline(never)]
#[cfg(not(feature = "std"))]
fn abort() -> ! {
    struct PanicOnDrop;
    impl Drop for PanicOnDrop {
        fn drop(&mut self) {
            panic!("fatal error: second panic")
        }
    }
    let _double_panicer = PanicOnDrop;
    panic!("fatal error: aborting via double panic");
}

#[cfg(feature = "std")]
use std::process::abort;

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn verify_type_pun_offsets() {
        assert_eq!(
            memoffset::offset_of!(ThinInner, strong),
            memoffset::offset_of!(ThinInnerStatic, strong),
        );
        assert_eq!(
            memoffset::offset_of!(ThinInner, len_flags),
            memoffset::offset_of!(ThinInnerStatic, len_flags),
        );
        assert_eq!(memoffset::offset_of!(ThinInner, len_flags), 0);
        assert_eq!(
            memoffset::offset_of!(ThinInner, data),
            memoffset::offset_of!(ThinInnerStatic, data),
        );
    }

    #[test]
    fn verify_type_pun_offsets_sasi_big_bufs() {
        fn sasi_layout_check<Buf>() {
            assert_eq!(
                memoffset::offset_of!(ThinInner, strong),
                memoffset::offset_of!(StaticArcStrInner::<Buf>, count),
            );
            assert_eq!(
                memoffset::offset_of!(ThinInner, len_flags),
                memoffset::offset_of!(StaticArcStrInner::<Buf>, len_flags),
            );
            assert_eq!(
                memoffset::offset_of!(ThinInner, data),
                memoffset::offset_of!(StaticArcStrInner::<Buf>, data),
            );
        }

        sasi_layout_check::<[u8; 0]>();
        sasi_layout_check::<[u8; 1]>();
        sasi_layout_check::<[u8; 2]>();
        sasi_layout_check::<[u8; 3]>();
        sasi_layout_check::<[u8; 4]>();
        sasi_layout_check::<[u8; 5]>();
        sasi_layout_check::<[u8; 15]>();
        sasi_layout_check::<[u8; 16]>();
        sasi_layout_check::<[u8; 64]>();
        sasi_layout_check::<[u8; 128]>();
        sasi_layout_check::<[u8; 1024]>();
        sasi_layout_check::<[u8; 4095]>();
        sasi_layout_check::<[u8; 4096]>();
    }
}

#[cfg(all(test, loom))]
mod loomtest {
    use super::ArcStr;
    use loom::sync::Arc;
    use loom::thread;
    #[test]
    fn cloning_threads() {
        loom::model(|| {
            let a = ArcStr::from("abcdefgh");
            let addr = a.as_ptr() as usize;

            let a1 = Arc::new(a);
            let a2 = a1.clone();

            let t1 = thread::spawn(move || {
                let b: ArcStr = (*a1).clone();
                assert_eq!(b.as_ptr() as usize, addr);
            });
            let t2 = thread::spawn(move || {
                let b: ArcStr = (*a2).clone();
                assert_eq!(b.as_ptr() as usize, addr);
            });

            t1.join().unwrap();
            t2.join().unwrap();
        });
    }
    #[test]
    fn drop_timing() {
        loom::model(|| {
            let a1 = (0..5)
                .map(|i| ArcStr::from(alloc::format!("s{}", i)))
                .cycle()
                .take(10)
                .collect::<alloc::vec::Vec<_>>();
            let a2 = a1.clone();

            let t1 = thread::spawn(move || {
                let mut a1 = a1;
                while let Some(s) = a1.pop() {
                    assert!(s.starts_with("s"));
                }
            });
            let t2 = thread::spawn(move || {
                let mut a2 = a2;
                while let Some(s) = a2.pop() {
                    assert!(s.starts_with("s"));
                }
            });

            t1.join().unwrap();
            t2.join().unwrap();
        });
    }
}
