//! Cây layout tiling nhị phân + tính rectangle cho từng pane và divider.

use std::collections::HashMap;

use ratatui::layout::Rect;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct PaneId(pub u64);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SplitId(pub u64);

/// Hướng chia: first|second nằm cạnh nhau (LeftRight) hay xếp chồng (TopBottom).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SplitDir {
    LeftRight,
    TopBottom,
}

pub enum Layout {
    Leaf(PaneId),
    Split {
        id: SplitId,
        dir: SplitDir,
        ratio: f32, // tỉ lệ phần `first`
        first: Box<Layout>,
        second: Box<Layout>,
    },
}

/// Vùng đường viền chia giữa hai pane con — dùng để hit-test khi kéo resize.
pub struct Divider {
    pub id: SplitId,
    pub dir: SplitDir,
    pub bounds: Rect, // rectangle của node Split (parent) — để quy đổi vị trí chuột → ratio
    pub line: Rect,   // vùng cột/hàng viền có thể kéo
}

impl Layout {
    /// Duyệt cây, gán rectangle cho mỗi leaf và thu thập các divider.
    pub fn compute(
        &self,
        area: Rect,
        areas: &mut HashMap<PaneId, Rect>,
        dividers: &mut Vec<Divider>,
    ) {
        match self {
            Layout::Leaf(id) => {
                areas.insert(*id, area);
            }
            Layout::Split {
                id,
                dir,
                ratio,
                first,
                second,
            } => match dir {
                SplitDir::LeftRight => {
                    let w = area.width;
                    if w < 2 {
                        first.compute(area, areas, dividers);
                        return;
                    }
                    let fw = ((w as f32 * ratio).round() as u16).clamp(1, w - 1);
                    let first_rect = Rect::new(area.x, area.y, fw, area.height);
                    let second_rect = Rect::new(area.x + fw, area.y, w - fw, area.height);
                    let line = Rect::new(area.x + fw - 1, area.y, 2, area.height);
                    dividers.push(Divider {
                        id: *id,
                        dir: *dir,
                        bounds: area,
                        line,
                    });
                    first.compute(first_rect, areas, dividers);
                    second.compute(second_rect, areas, dividers);
                }
                SplitDir::TopBottom => {
                    let h = area.height;
                    if h < 2 {
                        first.compute(area, areas, dividers);
                        return;
                    }
                    let fh = ((h as f32 * ratio).round() as u16).clamp(1, h - 1);
                    let first_rect = Rect::new(area.x, area.y, area.width, fh);
                    let second_rect = Rect::new(area.x, area.y + fh, area.width, h - fh);
                    let line = Rect::new(area.x, area.y + fh - 1, area.width, 2);
                    dividers.push(Divider {
                        id: *id,
                        dir: *dir,
                        bounds: area,
                        line,
                    });
                    first.compute(first_rect, areas, dividers);
                    second.compute(second_rect, areas, dividers);
                }
            },
        }
    }

    /// Đặt lại ratio cho node Split có `id` cho trước.
    pub fn set_ratio(&mut self, id: SplitId, ratio: f32) {
        if let Layout::Split {
            id: sid,
            ratio: r,
            first,
            second,
            ..
        } = self
        {
            if *sid == id {
                *r = ratio.clamp(0.05, 0.95);
            } else {
                first.set_ratio(id, ratio);
                second.set_ratio(id, ratio);
            }
        }
    }

    /// Thay leaf `target` bằng một Split mới chứa `target` và `new_pane`.
    pub fn split_leaf(
        &mut self,
        target: PaneId,
        new_split: SplitId,
        dir: SplitDir,
        new_pane: PaneId,
    ) -> bool {
        match self {
            Layout::Leaf(id) if *id == target => {
                let old = Layout::Leaf(*id);
                *self = Layout::Split {
                    id: new_split,
                    dir,
                    ratio: 0.5,
                    first: Box::new(old),
                    second: Box::new(Layout::Leaf(new_pane)),
                };
                true
            }
            Layout::Leaf(_) => false,
            Layout::Split { first, second, .. } => {
                first.split_leaf(target, new_split, dir, new_pane)
                    || second.split_leaf(target, new_split, dir, new_pane)
            }
        }
    }

    /// Leaf trái/trên cùng — dùng chọn focus mới sau khi đóng pane.
    pub fn first_leaf(&self) -> PaneId {
        match self {
            Layout::Leaf(id) => *id,
            Layout::Split { first, .. } => first.first_leaf(),
        }
    }

    /// Cây có chứa pane này không.
    pub fn contains(&self, target: PaneId) -> bool {
        match self {
            Layout::Leaf(id) => *id == target,
            Layout::Split { first, second, .. } => {
                first.contains(target) || second.contains(target)
            }
        }
    }

    /// Thu thập toàn bộ PaneId trong cây (dùng khi đóng cả tab).
    pub fn leaves(&self, out: &mut Vec<PaneId>) {
        match self {
            Layout::Leaf(id) => out.push(*id),
            Layout::Split { first, second, .. } => {
                first.leaves(out);
                second.leaves(out);
            }
        }
    }
}

/// Gỡ pane `target` khỏi cây; Split có con bị gỡ sẽ suy biến về con còn lại.
/// Trả `None` nếu cả cây rỗng (đóng pane cuối cùng).
pub fn remove_pane(node: Layout, target: PaneId) -> Option<Layout> {
    match node {
        Layout::Leaf(id) => {
            if id == target {
                None
            } else {
                Some(Layout::Leaf(id))
            }
        }
        Layout::Split {
            id,
            dir,
            ratio,
            first,
            second,
        } => {
            let f = remove_pane(*first, target);
            let s = remove_pane(*second, target);
            match (f, s) {
                (None, None) => None,
                (Some(x), None) => Some(x),
                (None, Some(y)) => Some(y),
                (Some(x), Some(y)) => Some(Layout::Split {
                    id,
                    dir,
                    ratio,
                    first: Box::new(x),
                    second: Box::new(y),
                }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_then_remove_collapses() {
        let mut l = Layout::Leaf(PaneId(1));
        assert!(l.split_leaf(PaneId(1), SplitId(1), SplitDir::LeftRight, PaneId(2)));
        // Sau split: root là Split(Leaf1, Leaf2)
        let mut areas = HashMap::new();
        let mut divs = Vec::new();
        l.compute(Rect::new(0, 0, 80, 24), &mut areas, &mut divs);
        assert_eq!(areas.len(), 2);
        assert_eq!(divs.len(), 1);
        // Gỡ pane 2 → suy biến về Leaf(1)
        let l = remove_pane(l, PaneId(2)).unwrap();
        assert_eq!(l.first_leaf(), PaneId(1));
        let mut areas = HashMap::new();
        let mut divs = Vec::new();
        l.compute(Rect::new(0, 0, 80, 24), &mut areas, &mut divs);
        assert_eq!(areas.len(), 1);
        assert!(divs.is_empty());
    }

    #[test]
    fn remove_last_pane_is_none() {
        let l = Layout::Leaf(PaneId(1));
        assert!(remove_pane(l, PaneId(1)).is_none());
    }
}
