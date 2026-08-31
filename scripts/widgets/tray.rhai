fn render(ctx) {
    let commands = [];
    let visible = ctx.tray.filter(|item| !item.hidden);
    let hidden = ctx.tray.filter(|item| item.hidden);
    let max_items = if ctx.config.contains("max_items") { ctx.config.max_items } else { 3 };
    let shown = visible.len.min(max_items);
    if shown == 0 && hidden.len == 0 {
        commands.push(#{ type: "text", x: 8, y: 0, w: ctx.width - 16, h: ctx.height, text: "No tray icons", font: "small", color: "text_dim", align: "center" });
        return #{ width: 104, interval: 500, commands: commands };
    }
    let x = 4;
    for index in 0..shown {
        let item = visible[index];
        let action = `@tray:left:${item.id}`;
        commands.push(#{ type: "rect", x: x, y: 5, w: 26, h: ctx.height - 10, radius: 8, hover_background: "surface_hover", action: action, right_action: `@tray:right:${item.id}`, double_action: `@tray:double:${item.id}` });
        if item.icon != 0 { commands.push(#{ type: "icon", x: x + 5, y: ctx.height / 2 - 8, w: 16, h: 16, icon: item.icon, action: action, right_action: `@tray:right:${item.id}`, double_action: `@tray:double:${item.id}` }); }
        else { commands.push(#{ type: "text", x: x + 2, y: 5, w: 22, h: ctx.height - 10, text: item.name, font: "small", color: "text_dim", align: "center", action: action, right_action: `@tray:right:${item.id}` }); }
        x += 30;
    }
    let overflow = visible.len - shown + hidden.len;
    if overflow > 0 { commands.push(#{ type: "text", x: x, y: 5, w: 30, h: ctx.height - 10, text: `+${overflow}`, font: "small", color: "text_dim", background: "surface", hover_background: "surface_hover", align: "center", radius: 8, action: "@tray_overflow" }); x += 34; }
    #{ width: x + 4, interval: 500, hover: "self", commands: commands }
}
