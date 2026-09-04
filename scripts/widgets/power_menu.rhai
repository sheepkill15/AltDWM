fn render(ctx) {
    let show_label = if ctx.config.contains("show_label") { ctx.config.show_label } else { false };
    let label = if ctx.config.contains("label") { ctx.config.label } else { "Power" };
    let width = if show_label { 88 } else { 40 };
    let commands = [
        #{ type: "rect", x: 4, y: 5, w: width - 8, h: ctx.height - 10, radius: 8, background: "surface", hover_background: "surface_hover", action: "@power_menu" },
        #{ type: "text", x: 5, y: 5, w: 30, h: ctx.height - 10, text: symbol(59368), font: "symbol", color: "text", align: "center", action: "@power_menu" }
    ];
    if show_label {
        commands.push(#{ type: "text", x: 35, y: 5, w: width - 40, h: ctx.height - 10, text: label, font: "strong", color: "text", align: "left", action: "@power_menu" });
    }
    #{ width: width, interval: 1000, hover: "self", commands: commands }
}
