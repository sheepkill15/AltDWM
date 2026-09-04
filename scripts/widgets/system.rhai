fn render(ctx) {
    let show_volume = if ctx.config.contains("show_volume") { ctx.config.show_volume } else { true };
    let show_network = if ctx.config.contains("show_network") { ctx.config.show_network } else { true };
    let show_battery = (if ctx.config.contains("show_battery") { ctx.config.show_battery } else { true }) && ctx.system.battery_present;
    let show_input = if ctx.config.contains("show_input") { ctx.config.show_input } else { true };
    // Every visible item occupies the same slot. Input uses one slot for its
    // globe and one for its language tag, so their centres follow the same
    // rhythm as volume, network, and battery.
    let slot = 30;
    let natural_width = ([show_volume, show_network, show_battery].filter(|value| value).len * slot + (if show_input { slot * 2 } else { 0 }) + 10).max(40);
    let up = if show_volume { "adjust_volume(5)" } else { "" };
    let down = if show_volume { "adjust_volume(-5)" } else { "" };
    let commands = [#{ type: "rect", x: 4, y: 5, w: ctx.width - 8, h: ctx.height - 10, radius: 8, background: "surface", hover_background: "surface_hover", action: "@quick_settings", scroll_up: up, scroll_down: down }];
    let x = 5;

    // Segoe Fluent Icons codepoints. `symbol(...)`, thresholds, colours, and
    // layout all live here so the shipped widget remains completely editable.
    if show_volume {
        let glyph = if ctx.system.volume < 0 { 59239 } else if ctx.system.muted || ctx.system.volume == 0 { 59794 } else if ctx.system.volume <= 33 { 59795 } else if ctx.system.volume <= 66 { 59796 } else { 59797 };
        commands.push(#{ type: "text", x: x, y: 5, w: slot, h: ctx.height - 10, text: symbol(glyph), font: "symbol", color: if ctx.system.muted || ctx.system.volume < 0 { "text_dim" } else { "text" }, align: "center", action: "@quick_settings", scroll_up: up, scroll_down: down });
        x += slot;
    }
    if show_network {
        let glyph = if ctx.system.network_kind == "wifi" { if ctx.system.network_signal <= 33 { 59506 } else if ctx.system.network_signal <= 66 { 59507 } else { 59508 } } else if ctx.system.network_kind == "wired" { 59449 } else if ctx.system.network_kind == "offline" { 59505 } else { 59752 };
        let color = if ctx.system.network_kind == "offline" { "#e06c5a" } else if ctx.system.network_kind == "unknown" { "text_dim" } else { "text" };
        commands.push(#{ type: "text", x: x, y: 5, w: slot, h: ctx.height - 10, text: symbol(glyph), font: "symbol", color: color, align: "center", action: "@quick_settings", scroll_up: up, scroll_down: down });
        x += slot;
    }
    if show_battery {
        let glyph = if ctx.system.battery < 0 { 59798 } else {
            let level = ctx.system.battery.min(100) * 10 / 100;
            if level == 10 { 59455 } else { 59472 + level }
        };
        let color = if ctx.system.battery < 0 { "text_dim" } else if !ctx.system.on_ac && ctx.system.battery <= 15 { "#e06c5a" } else { "text" };
        commands.push(#{ type: "text", x: x, y: 5, w: slot, h: ctx.height - 10, text: symbol(glyph), font: "symbol", color: color, align: "center", action: "@quick_settings", scroll_up: up, scroll_down: down });
        x += slot;
    }
    if show_input {
        commands.push(#{ type: "text", x: x, y: 5, w: slot, h: ctx.height - 10, text: symbol(62135), font: "symbol", color: "text", align: "center", action: "@quick_settings", scroll_up: up, scroll_down: down });
        commands.push(#{ type: "text", x: x + slot, y: 5, w: slot, h: ctx.height - 10, text: if ctx.system.input == "" { "--" } else { ctx.system.input }, font: "strong", color: "text", align: "center", action: "@quick_settings", scroll_up: up, scroll_down: down });
    }
    #{ width: natural_width, interval: 500, hover: "self", commands: commands }
}
