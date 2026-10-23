//! The size-class table: 16-byte steps to 256, then mimalloc's coarser
//! medium table — four classes per power of two — to 32 KiB (spec
//! section 9.1). Sizes above `MAX_SMALL` are large objects.

/// Number of size classes; class 0 holds 16-byte blocks.
pub const NUM_CLASSES: usize = 44;

/// The largest size served by a size class; anything larger is a large
/// object on a page run.
pub const MAX_SMALL: usize = 32 * 1024;

/// Block size of each class, in bytes. `[16, 32, … 256, 320, 384, 448,
/// 512, 640, … 32768]`.
pub const CLASS_SIZES: [u16; NUM_CLASSES] = build_table();

const fn build_table() -> [u16; NUM_CLASSES] {
    let mut sizes = [0u16; NUM_CLASSES];
    let mut i = 0;
    while i < 16 {
        sizes[i] = (16 * (i + 1)) as u16;
        i += 1;
    }
    // Four classes per power-of-two octave from 256 to 32 KiB: 1.25, 1.5,
    // 1.75 and 2 times the octave base, mimalloc's `mi_bin` table.
    let mut k = 8;
    while k <= 14 {
        let base = 1u32 << k;
        sizes[i] = (base + base / 4) as u16;
        sizes[i + 1] = (base + base / 2) as u16;
        sizes[i + 2] = (base + 3 * base / 4) as u16;
        sizes[i + 3] = (base * 2) as u16;
        i += 4;
        k += 1;
    }
    sizes
}

/// Block size served by `class`.
pub const fn class_size(class: usize) -> usize {
    CLASS_SIZES[class] as usize
}

/// The class whose blocks fit `size`. `size` must be nonzero and at most
/// `MAX_SMALL`.
pub fn class_of(size: usize) -> usize {
    debug_assert!(size > 0 && size <= MAX_SMALL);
    if size <= 256 {
        (size - 1) >> 4
    } else {
        16 + CLASS_SIZES[16..].partition_point(|&s| (s as usize) < size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_shape() {
        assert_eq!(class_size(0), 16);
        assert_eq!(class_size(15), 256);
        assert_eq!(class_size(NUM_CLASSES - 1), MAX_SMALL);
        // 16-byte steps to 256, mimalloc's medium table above.
        for c in 0..16 {
            assert_eq!(class_size(c), 16 * (c + 1));
        }
        assert_eq!(class_size(16), 320);
        assert_eq!(class_size(17), 384);
        assert_eq!(class_size(19), 512);
        assert_eq!(class_size(23), 1024);
        assert_eq!(class_size(31), 4096);
        assert_eq!(class_size(39), 16384);
        for s in CLASS_SIZES.windows(2) {
            assert!(s[0] < s[1]);
        }
        for &s in CLASS_SIZES.iter() {
            assert_eq!(s % 16, 0);
        }
    }

    #[test]
    fn classes_cover_every_small_size() {
        for size in 1..=MAX_SMALL {
            let class = class_of(size);
            let block = class_size(class);
            assert!(block >= size, "size {size} got class block {block}");
            if class > 0 {
                assert!(class_size(class - 1) < size);
            }
        }
        // Exact fits land on their own class.
        assert_eq!(class_of(1), 0);
        assert_eq!(class_of(16), 0);
        assert_eq!(class_of(17), 1);
        assert_eq!(class_of(256), 15);
        assert_eq!(class_of(257), 16);
        assert_eq!(class_of(320), 16);
        assert_eq!(class_of(321), 17);
        assert_eq!(class_of(MAX_SMALL), NUM_CLASSES - 1);
    }
}
