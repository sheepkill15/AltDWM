fn render(ctx) {
    #{ width: 118, interval: 1000, hover: "self", commands: [
        #{ type: "rect", x: 4, y: 5, w: ctx.width - 8, h: ctx.height - 10, radius: 8, hover_background: "surface_hover", action: "@quick_settings" },
        #{ type: "text", x: 12, y: 5, w: ctx.width - 24, h: ctx.height - 10, text: ctx.system.network, color: if ctx.system.connected { "text" } else { "text_dim" }, action: "@quick_settings" }
    ] }
}
