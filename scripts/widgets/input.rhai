fn render(ctx) {
    let label = if ctx.system.input == "" { "Input" } else { ctx.system.input };
    #{ width: 78, interval: 500, hover: "self", commands: [
        #{ type: "rect", x: 4, y: 5, w: ctx.width - 8, h: ctx.height - 10, radius: 8, hover_background: "surface_hover", action: "cycle_input", scroll_up: "cycle_input", scroll_down: "cycle_input" },
        #{ type: "text", x: 10, y: 5, w: ctx.width - 20, h: ctx.height - 10, text: label, font: "strong", color: "text", align: "center", action: "cycle_input", scroll_up: "cycle_input", scroll_down: "cycle_input" }
    ] }
}
