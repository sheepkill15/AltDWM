fn render(ctx) {
    let commands = [];
    let count = ctx.workspaces.len;
    if count == 0 { return #{ width: 8, interval: 250, commands: commands }; }
    let gap = 4;
    let cell = (ctx.width - gap * (count + 1)) / count;
    for index in 0..count {
        let item = ctx.workspaces[index];
        let x = gap + index * (cell + gap);
        let bg = if item.active { "accent" } else if item.occupied { "surface" } else { "panel" };
        commands.push(#{ type: "rect", x: x, y: 5, w: cell, h: ctx.height - 10, radius: 8, background: bg, hover_background: "surface_hover", action: `workspace(${item.number})`, scroll_up: "prev_workspace", scroll_down: "next_workspace" });
        commands.push(#{ type: "text", x: x, y: 5, w: cell, h: ctx.height - 10, text: item.number.to_string(), font: if item.active { "strong" } else { "body" }, color: if item.active || item.occupied { "text" } else { "text_dim" }, align: "center", action: `workspace(${item.number})`, scroll_up: "prev_workspace", scroll_down: "next_workspace" });
    }
    #{ width: count * 30 + 8, interval: 250, hover: "self", commands: commands }
}
