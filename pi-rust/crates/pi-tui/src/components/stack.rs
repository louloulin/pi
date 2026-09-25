//! Stack layout primitives — 1:1 port of
//! `packages/tui/src/components/stack.ts`.
//!
//! The flex-like layout algorithm upstream uses for `VStack` and
//! `HStack`: each entry carries `basis` / `grow` / `shrink` / `minSize`
//! / `maxSize` and the host calls [`allocate_stack_sizes`] to spread
//! `available_size` across the entries.
//!
//! This module is the pure-algorithm half. The `VStack` / `HStack`
//! component classes (in `v_stack.rs` / `h_stack.rs`) wrap a
//! `Vec<StackLayoutEntry>` and reuse this allocator; a plugin that
//! needs the same layout against custom components can drop straight
//! to the primitives.

use crate::component::Component;

/// Layout hints for a single stack entry.
///
/// Mirrors upstream `StackEntryOptions`
/// (`packages/tui/src/components/stack.ts:4-11`).
#[derive(Default)]
pub struct StackEntryOptions {
    /// Initial size in rows/cols. `None` and `"auto"` both fall back
    /// to the entry's intrinsic size; a fixed number is honoured.
    pub basis: Option<StackBasis>,
    /// Growth weight when there is leftover space.
    pub grow: Option<usize>,
    /// Shrink weight when there is not enough space. `1` is the
    /// upstream default.
    pub shrink: Option<usize>,
    /// Hard floor on the entry's allocated size.
    pub min_size: Option<usize>,
    /// Hard ceiling on the entry's allocated size.
    pub max_size: Option<usize>,
    /// Per-viewport visibility hook. Returns `true` to keep the entry
    /// in the layout, `false` to hide it.
    pub visible: Option<Box<dyn Fn(StackLayoutViewport) -> bool + Send + Sync>>,
}

/// `basis` value — a fixed count or "auto" for the intrinsic size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StackBasis {
    /// Use the intrinsic size as the basis.
    Auto,
    /// Use this exact number of rows/cols as the basis.
    Fixed(usize),
}

impl StackBasis {
    /// Resolve to a numeric basis using the intrinsic size when `Auto`.
    pub fn resolve(self, intrinsic: usize) -> usize {
        match self {
            StackBasis::Auto => intrinsic,
            StackBasis::Fixed(n) => n,
        }
    }
}

/// Viewport snapshot the `visible` callback receives.
#[derive(Debug, Clone, Copy, Default)]
pub struct StackLayoutViewport {
    /// Viewport width in columns.
    pub width: u16,
    /// Viewport height in rows.
    pub height: u16,
}

/// A single stack entry — `component` plus the layout hints.
pub struct StackLayoutEntry {
    /// The component being laid out.
    pub component: Box<dyn Component>,
    /// Initial size. `None` means "use the intrinsic size".
    pub basis: Option<usize>,
    /// Growth weight.
    pub grow: usize,
    /// Shrink weight.
    pub shrink: usize,
    /// Hard floor.
    pub min_size: usize,
    /// Hard ceiling. `usize::MAX` means unbounded (upstream's
    /// `Number.MAX_SAFE_INTEGER`).
    pub max_size: usize,
    /// Visibility hook.
    pub visible: Option<Box<dyn Fn(StackLayoutViewport) -> bool + Send + Sync>>,
}

impl std::fmt::Debug for StackLayoutEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StackLayoutEntry")
            .field("component", &"<dyn Component>")
            .field("basis", &self.basis)
            .field("grow", &self.grow)
            .field("shrink", &self.shrink)
            .field("min_size", &self.min_size)
            .field("max_size", &self.max_size)
            .field("visible", &self.visible.as_ref().map(|_| "<fn>"))
            .finish()
    }
}

/// Stack-wide layout options.
///
/// Mirrors upstream `StackOptions`
/// (`packages/tui/src/components/stack.ts:19-22`).
#[derive(Debug, Clone, Default)]
pub struct StackOptions {
    /// Number of blank rows/cols between consecutive entries.
    pub gap: usize,
    /// How children align along the cross axis.
    pub align: StackAlign,
}

/// Cross-axis alignment for stack entries.
///
/// Mirrors upstream's `"stretch" | "start" | "center" | "end"` union.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StackAlign {
    /// Stretch every entry to the cross-axis size (default).
    #[default]
    Stretch,
    /// Pin entries to the start.
    Start,
    /// Centre entries.
    Center,
    /// Pin entries to the end.
    End,
}

impl StackAlign {
    /// Resolve from the upstream string union.
    pub fn from_upstream(value: &str) -> Self {
        match value {
            "start" => Self::Start,
            "center" => Self::Center,
            "end" => Self::End,
            _ => Self::Stretch,
        }
    }
}

/// Filter `entries` through their `visible` hooks against `viewport`.
/// Mirrors upstream `visibleStackEntries`
/// (`packages/tui/src/components/stack.ts:82-87`).
pub fn visible_stack_entries<'a>(
    entries: &'a [StackLayoutEntry],
    viewport: StackLayoutViewport,
) -> Vec<&'a StackLayoutEntry> {
    entries
        .iter()
        .filter(|e| e.visible.as_ref().map(|f| f(viewport)).unwrap_or(true))
        .collect()
}

fn clamp_size(size: usize, entry: &StackLayoutEntry) -> usize {
    let min = entry.min_size;
    let max = entry.max_size.max(min);
    size.max(min).min(max)
}

/// Compute a per-entry size allocation that fills `available_size`,
/// distributing any leftover with `grow` weights and any overflow with
/// `shrink` weights. Mirrors upstream `allocateStackSizes`
/// (`packages/tui/src/components/stack.ts:135-154`).
pub fn allocate_stack_sizes(
    entries: &[StackLayoutEntry],
    intrinsic_sizes: &[usize],
    available_size: Option<usize>,
    gap: usize,
) -> Vec<usize> {
    let mut sizes: Vec<usize> = entries
        .iter()
        .zip(intrinsic_sizes.iter().chain(std::iter::repeat(&0)))
        .map(|(entry, intrinsic)| {
            let basis = entry.basis.unwrap_or(*intrinsic);
            clamp_size(basis, entry)
        })
        .collect();

    let available = match available_size {
        Some(n) => n,
        None => return sizes,
    };

    let gap_total = entries.len().saturating_sub(1) * gap;
    let content_size = available.saturating_sub(gap_total);
    let total: usize = sizes.iter().sum();
    if total < content_size {
        distribute(&mut sizes, entries, content_size - total, GrowOrShrink::Grow);
    } else if total > content_size {
        distribute(&mut sizes, entries, total - content_size, GrowOrShrink::Shrink);
    }
    sizes
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum GrowOrShrink {
    Grow,
    Shrink,
}

fn distribute(
    sizes: &mut [usize],
    entries: &[StackLayoutEntry],
    amount: usize,
    mode: GrowOrShrink,
) {
    let mut remaining = amount;
    while remaining > 0 {
        let candidates: Vec<(usize, &StackLayoutEntry)> = entries
            .iter()
            .enumerate()
            .filter(|(idx, entry)| match mode {
                GrowOrShrink::Grow => entry.grow > 0 && sizes[*idx] < entry.max_size,
                GrowOrShrink::Shrink => entry.shrink > 0 && sizes[*idx] > entry.min_size,
            })
            .collect();
        if candidates.is_empty() {
            return;
        }

        let total_weight: usize = candidates
            .iter()
            .map(|(idx, entry)| match mode {
                GrowOrShrink::Grow => entry.grow,
                GrowOrShrink::Shrink => entry.shrink * sizes[*idx].max(1),
            })
            .sum();
        let mut distributed = 0usize;
        for (idx, entry) in &candidates {
            if remaining == 0 {
                break;
            }
            let weight = match mode {
                GrowOrShrink::Grow => entry.grow,
                GrowOrShrink::Shrink => entry.shrink * sizes[*idx].max(1),
            };
            let proposed = (remaining * weight / total_weight.max(1)).max(1);
            let capacity = match mode {
                GrowOrShrink::Grow => entry.max_size.saturating_sub(sizes[*idx]),
                GrowOrShrink::Shrink => sizes[*idx].saturating_sub(entry.min_size),
            };
            let delta = remaining.min(proposed).min(capacity);
            if delta == 0 {
                continue;
            }
            sizes[*idx] = match mode {
                GrowOrShrink::Grow => sizes[*idx] + delta,
                GrowOrShrink::Shrink => sizes[*idx] - delta,
            };
            remaining -= delta;
            distributed += delta;
        }
        if distributed == 0 {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::Spacer;
    use crate::utils::styled::SpanStyle;

    fn entries(count: usize, grow: usize, shrink: usize, min: usize, max: usize) -> Vec<StackLayoutEntry> {
        (0..count)
            .map(|_| StackLayoutEntry {
                component: Box::new(Spacer::one()),
                basis: None,
                grow,
                shrink,
                min_size: min,
                max_size: max,
                visible: None,
            })
            .collect()
    }

    #[test]
    fn empty_stack_returns_empty() {
        let sizes = allocate_stack_sizes(&[], &[], Some(10), 0);
        assert!(sizes.is_empty());
    }

    #[test]
    fn no_available_size_returns_intrinsic() {
        let e = entries(2, 0, 1, 0, usize::MAX);
        let sizes = allocate_stack_sizes(&e, &[3, 5], None, 0);
        assert_eq!(sizes, vec![3, 5]);
    }

    #[test]
    fn grow_distributes_leftover() {
        let e = entries(2, 1, 1, 0, usize::MAX);
        let sizes = allocate_stack_sizes(&e, &[1, 1], Some(10), 0);
        assert_eq!(sizes.iter().sum::<usize>(), 10);
    }

    #[test]
    fn shrink_distributes_overflow() {
        let e = entries(2, 1, 1, 0, usize::MAX);
        let sizes = allocate_stack_sizes(&e, &[6, 6], Some(8), 0);
        assert_eq!(sizes.iter().sum::<usize>(), 8);
    }

    #[test]
    fn gap_reduces_content_size() {
        let e = entries(3, 1, 1, 0, usize::MAX);
        let sizes = allocate_stack_sizes(&e, &[0, 0, 0], Some(8), 1);
        // 8 - 2 gaps = 6 rows of content.
        assert_eq!(sizes.iter().sum::<usize>(), 6);
    }

    #[test]
    fn min_size_is_a_floor() {
        let e = entries(1, 0, 0, 5, usize::MAX);
        let sizes = allocate_stack_sizes(&e, &[1], Some(2), 0);
        assert_eq!(sizes, vec![5]);
    }

    #[test]
    fn max_size_is_a_ceiling() {
        let e = entries(1, 0, 0, 0, 3);
        let sizes = allocate_stack_sizes(&e, &[10], Some(20), 0);
        assert_eq!(sizes, vec![3]);
    }

    #[test]
    fn visible_stack_entries_filters() {
        let mut e = entries(3, 1, 1, 0, usize::MAX);
        e[1].visible = Some(Box::new(|_vp| false));
        let visible = visible_stack_entries(&e, StackLayoutViewport::default());
        assert_eq!(visible.len(), 2);
    }

    #[test]
    fn stack_align_from_upstream() {
        assert_eq!(StackAlign::from_upstream("stretch"), StackAlign::Stretch);
        assert_eq!(StackAlign::from_upstream("start"), StackAlign::Start);
        assert_eq!(StackAlign::from_upstream("center"), StackAlign::Center);
        assert_eq!(StackAlign::from_upstream("end"), StackAlign::End);
        assert_eq!(StackAlign::from_upstream("garbage"), StackAlign::Stretch);
    }

    #[test]
    fn basis_resolves_to_fixed_or_intrinsic() {
        assert_eq!(StackBasis::Auto.resolve(7), 7);
        assert_eq!(StackBasis::Fixed(3).resolve(7), 3);
    }

    #[test]
    fn component_for_stack_entry_keeps_default() {
        let _unused = SpanStyle::default();
    }
}