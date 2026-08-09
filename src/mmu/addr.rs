use core::{fmt, mem::Alignment, ops::Range};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PageType {
    Small,
    Large,
    Huge,
}

impl PageType {
    pub const fn size(self) -> usize {
        match self {
            PageType::Small => 4096,
            PageType::Large => 2 * 1024 * 1024,
            PageType::Huge => 1024 * 1024 * 1024,
        }
    }

    pub const fn alignment(self) -> Alignment {
        Alignment::new(self.size()).expect("PageType::size should always return a power of two")
    }

    pub const fn mask(self) -> usize {
        !(self.size() - 1)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct PhysicalAddr(u64);

impl PhysicalAddr {
    pub const fn new(addr: u64) -> Self {
        // TODO: validate 56-bit physical addr
        Self(addr)
    }

    pub fn get(self) -> u64 {
        self.0
    }

    pub fn ppn(self) -> u64 {
        self.0 >> 12
    }

    pub fn from_ppn(ppn: u64) -> Self {
        Self::new(ppn << 12)
    }

    pub fn is_aligned(self, page_type: PageType) -> bool {
        self.0.is_multiple_of(page_type.size() as _)
    }

    pub fn add(self, by: usize) -> Self {
        Self::new(self.0.strict_add(by as _))
    }
}

impl fmt::Debug for PhysicalAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("PhysicalAddr")
            .field_with(|f| write!(f, "{:#010x}", self.0))
            .finish()
    }
}

impl fmt::Display for PhysicalAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#010x}", self.0)
    }
}

impl From<PhysicalAddr> for usize {
    fn from(value: PhysicalAddr) -> Self {
        value.get() as _
    }
}

impl From<usize> for PhysicalAddr {
    fn from(value: usize) -> Self {
        Self::new(value as _)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct VirtualAddr(usize);

impl VirtualAddr {
    // TODO: add virtual address validation (canonical lower or upper half)
    pub const fn new(addr: usize) -> Self {
        Self(addr)
    }

    pub fn expose_provenance<T>(ptr: *const T) -> Self {
        Self::new(ptr.expose_provenance())
    }

    pub fn as_ptr(self) -> *const u8 {
        core::ptr::with_exposed_provenance(self.0)
    }

    pub fn get(self) -> usize {
        self.0
    }

    pub fn vpn0(self) -> usize {
        (self.0 >> 12) & 0x1ff
    }

    pub fn vpn1(self) -> usize {
        (self.0 >> 21) & 0x1ff
    }

    pub fn vpn2(self) -> usize {
        (self.0 >> 30) & 0x1ff
    }

    pub fn is_aligned(self, page_type: PageType) -> bool {
        self.0.is_multiple_of(page_type.size())
    }

    pub fn add(self, offset: usize) -> Self {
        Self::new(self.0.strict_add(offset))
    }
}

impl fmt::Debug for VirtualAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("VirtualAddr")
            .field_with(|f| write!(f, "{:#016x}", self.0))
            .finish()
    }
}

impl<T> From<*const T> for VirtualAddr {
    fn from(ptr: *const T) -> Self {
        Self::expose_provenance(ptr)
    }
}

impl From<usize> for VirtualAddr {
    fn from(value: usize) -> Self {
        Self::new(value)
    }
}

impl fmt::Display for VirtualAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#016x}", self.0)
    }
}

impl From<VirtualAddr> for usize {
    fn from(value: VirtualAddr) -> Self {
        value.get()
    }
}

#[derive(Debug, Copy, Clone)]
pub struct AddressRange<T> {
    pub start: T,
    pub end: T,
}

impl<T> AddressRange<T> {
    /// Creates a new address range with addresses `[from, until)`.
    ///
    /// `start` is the first address in the range.
    /// `end` is the first address outside the range.
    pub fn new(start: T, end: T) -> Self {
        Self { start, end }
    }

    pub fn map<U>(self, f: impl Fn(T) -> U) -> AddressRange<U> {
        AddressRange {
            start: f(self.start),
            end: f(self.end),
        }
    }
}

impl<T: Copy + Into<usize>> AddressRange<T> {
    pub fn size(&self) -> usize {
        self.end.into().saturating_sub(self.start.into())
    }
}

impl<T> AddressRange<T>
where
    T: Copy + From<usize> + Into<usize>,
{
    pub fn with_aligned_end(self, page_type: PageType) -> Self {
        Self::new(
            self.start,
            self.end.into().next_multiple_of(page_type.size()).into(),
        )
    }

    pub fn page(start: T, page_type: PageType) -> Self {
        Self::new(start, start.into().strict_add(page_type.size()).into())
    }
}

impl<T: fmt::Display> fmt::Display for AddressRange<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
}

impl<T: Into<VirtualAddr>> From<Range<T>> for AddressRange<VirtualAddr> {
    fn from(range: Range<T>) -> Self {
        Self::new(range.start.into(), range.end.into())
    }
}

impl<T: Into<PhysicalAddr>> From<Range<T>> for AddressRange<PhysicalAddr> {
    fn from(range: Range<T>) -> Self {
        Self::new(range.start.into(), range.end.into())
    }
}
