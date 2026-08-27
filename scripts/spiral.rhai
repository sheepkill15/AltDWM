// AltDWM custom layout — scripts/spiral.rhai
//
// Fibonacci spiral: each window takes half of the space that remains, and the
// split alternates between vertical and horizontal. The last window claims
// whatever is left, so the area is always filled exactly.
//
// Define `fn layout(n, left, top, right, bottom, gap)` returning an array of
// rect maps. Called when general.layout = "spiral" and
// [layouts.spiral] script = "scripts/spiral.rhai".
//
// Rhai note: bindings are `let`, never `let mut` — `let` is already mutable.

fn layout(n, left, top, right, bottom, gap) {
    let rects = [];
    if n < 1 {
        return rects;
    }

    // One inner rect inset by `gap` on all four sides, so the outer margin is
    // symmetric no matter how the interior is divided.
    let x = left + gap;
    let y = top + gap;
    let w = right - left - gap * 2;
    let h = bottom - top - gap * 2;
    if w < 1 { w = 1; }
    if h < 1 { h = 1; }

    let i = 0;
    while i < n {
        if i == n - 1 {
            // Last window fills the remainder exactly.
            rects.push(#{ "left": x, "top": y, "right": x + w, "bottom": y + h });
        } else if i % 2 == 0 {
            // Vertical cut: take the left half, continue in the right half.
            let cut = (w - gap) / 2;
            if cut < 1 { cut = 1; }
            rects.push(#{ "left": x, "top": y, "right": x + cut, "bottom": y + h });
            x = x + cut + gap;
            w = w - cut - gap;
            if w < 1 { w = 1; }
        } else {
            // Horizontal cut: take the top half, continue below.
            let cut = (h - gap) / 2;
            if cut < 1 { cut = 1; }
            rects.push(#{ "left": x, "top": y, "right": x + w, "bottom": y + cut });
            y = y + cut + gap;
            h = h - cut - gap;
            if h < 1 { h = 1; }
        }
        i = i + 1;
    }

    rects
}
