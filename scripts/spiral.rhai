// AltDWM custom layout — scripts/spiral.rhai
// This is Rhai code. Define fn layout(n, left, top, right, bottom, gap) returning array of rect maps.
// Called when general.layout = "spiral" and [layouts.spiral] script = "scripts/spiral.rhai"
// Example: MasterStack in Rhai (60% master, stack right) — replace with any algorithm

fn layout(n, left, top, right, bottom, gap) {
    let rects = [];
    let width = right - left;
    let height = bottom - top;

    if n == 1 {
        rects.push(#{"left": left+gap, "top": top+gap, "right": right-gap, "bottom": bottom-gap});
    } else {
        let master_w = width * 60 / 100 - gap / 2;
        let stack_w = width - master_w - gap*2;
        // master
        rects.push(#{"left": left+gap, "top": top+gap, "right": left+gap+master_w, "bottom": bottom-gap});
        // stack
        let stack_x = left+gap+master_w+gap;
        let stack_count = n - 1;
        let total_h = height - gap*2;
        let gap_total = gap * (stack_count - 1);
        let win_h = (total_h - gap_total) / stack_count;
        let y = top + gap;
        let i = 0;
        while i < stack_count {
            let b = y + win_h;
            if i == stack_count - 1 {
                b = bottom - gap;
            }
            rects.push(#{"left": stack_x, "top": y, "right": stack_x+stack_w, "bottom": b});
            y = y + win_h + gap;
            i = i + 1;
        }
    }
    rects
}

// alternative spiral example (commented):
// fn layout(n, left, top, right, bottom, gap) {
//     // Fibonacci spiral — each window splits remaining area
//     let rects = [];
//     let mut x = left+gap;
//     let mut y = top+gap;
//     let mut w = right-left - gap*2;
//     let mut h = bottom-top - gap*2;
//     let mut i = 0;
//     while i < n {
//         let is_h = i % 2 == 0;
//         if i == n-1 {
//             rects.push(#{"left": x, "top": y, "right": x+w, "bottom": y+h});
//         } else if is_h {
//             let cw = w/2 - gap/2;
//             rects.push(#{"left": x, "top": y, "right": x+cw, "bottom": y+h});
//             x += cw + gap;
//             w -= cw + gap;
//         } else {
//             let ch = h/2 - gap/2;
//             rects.push(#{"left": x, "top": y, "right": x+w, "bottom": y+ch});
//             y += ch + gap;
//             h -= ch + gap;
//         }
//         i += 1;
//     }
//     rects
// }
