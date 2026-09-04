fn render(ctx) {
    let value = ctx.system.battery;
    let label = if !ctx.system.battery_present { "No battery" } else if value < 0 { "Battery unknown" } else if ctx.system.charging { `Charging ${value}%` } else { `Battery ${value}%` };
    #{ width: 112, interval: 1000, hover: "self", commands: [
        #{ type: "rect", x: 4, y: 5, w: ctx.width - 8, h: ctx.height - 10, radius: 8, hover_background: "surface_hover", action: "@quick_settings" },
        #{ type: "text", x: 12, y: 5, w: ctx.width - 24, h: ctx.height - 10, text: label, color: if value < 0 { "text_dim" } else { "text" }, action: "@quick_settings" }
    ] }
}
