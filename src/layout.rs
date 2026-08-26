use windows::Win32::Foundation::RECT;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Layout {
    /// Master on left 60%, stack on right
    MasterStack,
    /// Grid - auto rows/cols
    Grid,
    /// Fullscreen monocle (one window maximized)
    Monocle,
    /// Floating - do nothing
    Floating,
}

impl Layout {
    pub fn name(&self) -> &'static str {
        match self {
            Layout::MasterStack => "MasterStack",
            Layout::Grid => "Grid",
            Layout::Monocle => "Monocle",
            Layout::Floating => "Floating",
        }
    }
}

/// Compute tiled rectangles for `n` windows within `area`
/// Returns vec of RECTs in same order as input windows
pub fn compute_layout(n: usize, area: RECT, gap: i32, layout: Layout) -> Vec<RECT> {
    if n == 0 {
        return Vec::new();
    }
    match layout {
        Layout::Floating => Vec::new(),
        Layout::Monocle => vec![shrink_rect(area, gap); n],
        Layout::Grid => grid_layout(n, area, gap),
        Layout::MasterStack => master_stack_layout(n, area, gap),
    }
}

fn shrink_rect(r: RECT, gap: i32) -> RECT {
    RECT {
        left: r.left + gap,
        top: r.top + gap,
        right: r.right - gap,
        bottom: r.bottom - gap,
    }
}

fn master_stack_layout(n: usize, area: RECT, gap: i32) -> Vec<RECT> {
    if n == 1 {
        return vec![shrink_rect(area, gap / 2)];
    }

    let width = area.right - area.left;
    let height = area.bottom - area.top;

    let master_w = width * 60 / 100 - gap / 2;
    let stack_w = width - master_w - gap * 2;

    let mut rects = Vec::with_capacity(n);

    rects.push(RECT {
        left: area.left + gap,
        top: area.top + gap,
        right: area.left + gap + master_w,
        bottom: area.bottom - gap,
    });

    let stack_x = area.left + gap + master_w + gap;
    let stack_count = n - 1;
    let total_stack_h = height - gap * 2;
    let gap_total = gap * (stack_count as i32 - 1);
    let win_h = (total_stack_h - gap_total) / stack_count as i32;

    for i in 0..stack_count {
        let y = area.top + gap + i as i32 * (win_h + gap);
        let mut bottom = y + win_h;
        if i == stack_count - 1 {
            bottom = area.bottom - gap;
        }
        rects.push(RECT {
            left: stack_x,
            top: y,
            right: stack_x + stack_w,
            bottom,
        });
    }

    rects
}

fn grid_layout(n: usize, area: RECT, gap: i32) -> Vec<RECT> {
    let cols = (n as f64).sqrt().ceil() as usize;
    let rows = (n + cols - 1) / cols;

    let width = area.right - area.left;
    let height = area.bottom - area.top;

    let cell_w = (width - gap * (cols as i32 + 1)) / cols as i32;
    let cell_h = (height - gap * (rows as i32 + 1)) / rows as i32;

    let mut rects = Vec::with_capacity(n);
    for i in 0..n {
        let col = i % cols;
        let row = i / cols;
        let left = area.left + gap + col as i32 * (cell_w + gap);
        let top = area.top + gap + row as i32 * (cell_h + gap);
        rects.push(RECT {
            left,
            top,
            right: left + cell_w,
            bottom: top + cell_h,
        });
    }
    rects
}
