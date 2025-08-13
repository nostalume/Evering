pub const fn align_down(n: usize, align: usize) -> usize {
    if align.is_power_of_two() {
        n & !(align - 1)
    } else if align == 0 {
        n
    } else {
        panic!("`align` must be a power of 2");
    }
}

/// Align upwards. Returns the smallest x with alignment `align`
/// so that x >= addr.
///
/// # Safety
///
/// `align` must be a power of 2.
pub const fn align_up(n: usize, align: usize) -> usize {
    align_down(n + align - 1, align)
}
