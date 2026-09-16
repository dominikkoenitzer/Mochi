//! Where the tabs of a stackbar go. Pure maths, no Win32.

use mochi_core::Rect;

/// The geometry of one stackbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TabLayout {
    /// The bar itself, in physical screen pixels.
    pub bar: Rect,
    /// How wide one tab is.
    pub tab_width: i32,
    /// How many tabs there are.
    pub count: usize,
}

impl TabLayout {
    /// Lays out `count` tabs along the top of `container`.
    ///
    /// Tabs are `preferred_width` wide until they stop fitting, then they share
    /// the bar equally. A bar with no room left gives every tab zero width,
    /// which the painter skips.
    #[must_use]
    pub fn new(container: Rect, count: usize, height: i32, preferred_width: i32) -> Self {
        let height = height.max(0).min(container.height().max(0));
        let bar = Rect::new(
            container.left,
            container.top,
            container.right,
            container.top + height,
        );

        let available = bar.width().max(0);
        let wanted = preferred_width.max(0);
        let tab_width = if count == 0 {
            0
        } else {
            let fair = available / i32::try_from(count).unwrap_or(i32::MAX);
            wanted.min(fair)
        };

        Self {
            bar,
            tab_width,
            count,
        }
    }

    /// `true` when there is nothing to draw.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.count == 0 || self.tab_width <= 0 || self.bar.is_empty()
    }

    /// The left and right edge of one tab, relative to the left of the bar.
    #[must_use]
    pub fn tab_bounds(&self, index: usize) -> Option<(i32, i32)> {
        if index >= self.count || self.tab_width <= 0 {
            return None;
        }
        let left = i32::try_from(index)
            .unwrap_or(i32::MAX)
            .saturating_mul(self.tab_width);
        Some((left, left + self.tab_width))
    }

    /// One tab in screen coordinates.
    #[must_use]
    pub fn tab_rect(&self, index: usize) -> Option<Rect> {
        let (left, right) = self.tab_bounds(index)?;
        Some(Rect::new(
            self.bar.left + left,
            self.bar.top,
            self.bar.left + right,
            self.bar.bottom,
        ))
    }

    /// Which tab a click at `x` pixels from the left of the bar landed on.
    #[must_use]
    pub fn hit(&self, x: i32) -> Option<usize> {
        if x < 0 || self.tab_width <= 0 {
            return None;
        }
        let index = usize::try_from(x / self.tab_width).ok()?;
        (index < self.count).then_some(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONTAINER: Rect = Rect::new(100, 200, 1100, 900);

    #[test]
    fn the_bar_sits_along_the_top_of_the_container() {
        let layout = TabLayout::new(CONTAINER, 3, 40, 200);
        assert_eq!(layout.bar, Rect::new(100, 200, 1100, 240));
        assert_eq!(layout.bar.height(), 40);
    }

    #[test]
    fn tabs_take_their_preferred_width_when_they_fit() {
        let layout = TabLayout::new(CONTAINER, 3, 40, 200);
        assert_eq!(layout.tab_width, 200);
        assert_eq!(layout.tab_rect(0), Some(Rect::new(100, 200, 300, 240)));
        assert_eq!(layout.tab_rect(1), Some(Rect::new(300, 200, 500, 240)));
        assert_eq!(layout.tab_rect(2), Some(Rect::new(500, 200, 700, 240)));
        assert_eq!(layout.tab_rect(3), None);
    }

    #[test]
    fn tabs_share_the_bar_once_they_stop_fitting() {
        let layout = TabLayout::new(CONTAINER, 10, 40, 200);
        assert_eq!(layout.tab_width, 100, "1000 px across ten tabs");
        let last = layout.tab_rect(9).unwrap();
        assert!(
            last.right <= CONTAINER.right,
            "the tabs stay inside the bar"
        );
    }

    #[test]
    fn a_click_finds_its_tab() {
        let layout = TabLayout::new(CONTAINER, 3, 40, 200);
        assert_eq!(layout.hit(0), Some(0));
        assert_eq!(layout.hit(199), Some(0));
        assert_eq!(layout.hit(200), Some(1));
        assert_eq!(layout.hit(599), Some(2));
        assert_eq!(layout.hit(600), None, "past the last tab is not a tab");
        assert_eq!(layout.hit(-5), None);
    }

    #[test]
    fn a_bar_with_no_tabs_draws_nothing() {
        let layout = TabLayout::new(CONTAINER, 0, 40, 200);
        assert!(layout.is_empty());
        assert_eq!(layout.tab_bounds(0), None);
        assert_eq!(layout.hit(10), None);
    }

    #[test]
    fn a_container_shorter_than_the_bar_clamps_it() {
        let squashed = Rect::new(0, 0, 500, 20);
        let layout = TabLayout::new(squashed, 2, 40, 200);
        assert_eq!(
            layout.bar.height(),
            20,
            "the bar never leaves the container"
        );
    }

    #[test]
    fn an_empty_container_produces_an_empty_layout() {
        let layout = TabLayout::new(Rect::new(0, 0, 0, 0), 2, 40, 200);
        assert!(layout.is_empty());
    }
}
