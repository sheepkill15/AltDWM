fn render(ctx) {
    let label = if ctx.config.contains("label") { ctx.config.label } else { "AltDWM" };
    let icon = if ctx.config.contains("icon") { ctx.config.icon } else { "◆" };
    let action = if ctx.config.contains("action") { ctx.config.action } else if ctx.config.contains("command") { ctx.config.command } else { "command_center" };
    #{ width: 112, interval: 1000, hover: "self", commands: [
        #{ type: "rect", x: 4, y: 5, w: ctx.width - 8, h: ctx.height - 10, radius: 8, background: "surface", hover_background: "accent", action: action },
        #{ type: "text", x: 12, y: 5, w: 18, h: ctx.height - 10, text: icon, font: "strong", color: "text", align: "center", action: action },
        #{ type: "text", x: 34, y: 5, w: ctx.width - 44, h: ctx.height - 10, text: label, font: "strong", color: "text", action: action }
    ] }
}
