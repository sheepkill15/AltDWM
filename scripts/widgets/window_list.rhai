fn render(ctx) {
    let commands = [];
    let count = ctx.windows.len;
    if count == 0 { return #{ width: 0, interval: 250, commands: commands }; }
    let gap = 4;
    let cell = ((ctx.width - gap * (count + 1)) / count).max(1).min(220);
    for index in 0..count {
        let window = ctx.windows[index];
        let x = gap + index * (cell + gap);
        let action = `@window:${window.id}`;
        commands.push(#{ type: "rect", x: x, y: 5, w: cell, h: ctx.height - 10, radius: 8, background: if window.active { "surface" } else { "panel" }, hover_background: "surface_hover", action: action });
        if window.icon != 0 { commands.push(#{ type: "icon", x: x + 8, y: ctx.height / 2 - 8, w: 16, h: 16, icon: window.icon, action: action }); }
        commands.push(#{ type: "text", x: x + 30, y: 5, w: cell - 38, h: ctx.height - 10, text: window.title, font: if window.active { "strong" } else { "body" }, color: if window.minimized { "text_dim" } else { "text" }, action: action });
        if window.active { commands.push(#{ type: "rect", x: x + 10, y: ctx.height - 7, w: cell - 20, h: 2, radius: 1, background: "accent", action: action }); }
    }
    #{ width: 0, interval: 250, hover: "self", commands: commands }
}
