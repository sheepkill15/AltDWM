// AltDWM custom widget — scripts/cpu.rhai
// Return a string; Rhai expression last value is displayed
// Available: launch(cmd), log(msg), get_cpu_usage(), focused_title(), retile(), set_layout(name)
let cpu = get_cpu_usage();
// In Rhai, string interpolation via `... ${var}` with backticks is not native — use + concat
"CPU " + cpu + "%"
