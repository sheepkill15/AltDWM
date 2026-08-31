fn render(ctx) {
    let count = 0;
    for window in ctx.windows { if !window.floating { count += 1; } }
    let status = if ctx.tiling { `${count} managed` } else { "paused" };
    let next = if ctx.layout == "MasterStack" { "Grid" } else if ctx.layout == "Grid" { "Monocle" } else if ctx.layout == "Monocle" { "Floating" } else { "MasterStack" };
    let action = `set_layout("${next}")`;
    let commands = [#{ type: "rect", x: 4, y: 5, w: ctx.width - 8, h: ctx.height - 10, radius: 8, background: "surface", hover_background: "surface_hover", action: action }];
    commands.push(#{ type: "rect", x: 14, y: ctx.height / 2 - 4, w: 8, h: 8, radius: 2, background: if ctx.tiling { "accent" } else { "text_dim" }, action: action });
    if ctx.height >= 34 {
        commands.push(#{ type: "text", x: 30, y: 2, w: ctx.width - 40, h: ctx.height / 2, text: ctx.layout, font: "strong", color: "text", action: action });
        commands.push(#{ type: "text", x: 30, y: ctx.height / 2, w: ctx.width - 40, h: ctx.height / 2 - 2, text: status, font: "small", color: "text_dim", action: action });
    } else {
        commands.push(#{ type: "text", x: 30, y: 0, w: ctx.width - 40, h: ctx.height, text: `${ctx.layout} · ${status}`, color: "text", action: action });
    }
    #{ width: 178, interval: 250, hover: "self", commands: commands }
}
