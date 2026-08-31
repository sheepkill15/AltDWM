// Editable built-in clock. All geometry is in 96-DPI logical pixels.
fn render(ctx) {
    let time_format = if ctx.config.contains("format") { ctx.config.format } else { "%H:%M" };
    let two_lines = ctx.height >= 34;
    let commands = [];
    if two_lines {
        commands.push(#{ type: "text", x: 10, y: 2, w: ctx.width - 20, h: ctx.height / 2, text: format_time(time_format), font: "strong", color: "text", align: "center" });
        commands.push(#{ type: "text", x: 10, y: ctx.height / 2, w: ctx.width - 20, h: ctx.height / 2 - 2, text: format_time("%a %d %b"), font: "small", color: "text_dim", align: "center" });
    } else {
        commands.push(#{ type: "text", x: 10, y: 0, w: ctx.width - 20, h: ctx.height, text: format_time(time_format), font: "strong", color: "text", align: "center" });
    }
    #{ width: 112, interval: 1000, hover: if ctx.config.contains("action") { "whole" } else { "none" }, commands: commands }
}
