//! Screen geometry used in snapshots and hit tests.

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    /// True when `p` lies inside the rectangle (right and bottom edges are exclusive).
    pub fn contains(&self, p: Point) -> bool {
        let right = i64::from(self.x) + i64::from(self.w);
        let bottom = i64::from(self.y) + i64::from(self.h);
        p.x >= self.x && p.y >= self.y && i64::from(p.x) < right && i64::from(p.y) < bottom
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_contains_is_half_open() {
        let r = Rect {
            x: 10,
            y: 10,
            w: 5,
            h: 5,
        };
        assert!(r.contains(Point { x: 10, y: 10 }));
        assert!(r.contains(Point { x: 14, y: 14 }));
        assert!(!r.contains(Point { x: 15, y: 14 }));
        assert!(!r.contains(Point { x: 9, y: 10 }));
    }

    #[test]
    fn rect_contains_does_not_overflow() {
        let r = Rect {
            x: i32::MAX,
            y: 0,
            w: u32::MAX,
            h: 1,
        };
        assert!(r.contains(Point { x: i32::MAX, y: 0 }));
    }
}
