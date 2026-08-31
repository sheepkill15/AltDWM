fn render(ctx) {
    let title = if ctx.focused_title == "" { "AltDWM" } else { ctx.focused_title };
    let max_len = if ctx.config.contains("max_len") { ctx.config.max_len } else { 64 };
    title = truncate_text(title, max_len);
    #{ width: 0, interval: 250, commands: [
        #{ type: "text", x: 10, y: 0, w: ctx.width - 20, h: ctx.height, text: title, color: "text" }
    ] }
}
