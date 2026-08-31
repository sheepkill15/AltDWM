fn render(ctx) {
    let value = ctx.system.volume;
    let label = if value < 0 { "Audio unavailable" } else if ctx.system.muted { `Muted ${value}%` } else { `Volume ${value}%` };
    #{ width: 112, interval: 500, hover: "self", commands: [
        #{ type: "rect", x: 4, y: 5, w: ctx.width - 8, h: ctx.height - 10, radius: 8, hover_background: "surface_hover", action: "@quick_settings", scroll_up: "adjust_volume(5)", scroll_down: "adjust_volume(-5)" },
        #{ type: "text", x: 12, y: 5, w: ctx.width - 24, h: ctx.height - 10, text: label, color: if value < 0 { "text_dim" } else { "text" }, action: "@quick_settings", scroll_up: "adjust_volume(5)", scroll_down: "adjust_volume(-5)" }
    ] }
}
