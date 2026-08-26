// AltDWM custom widget — scripts/cpu.rhai
// Return a string; Rhai expression last value is displayed
// Available: launch(cmd), log(msg), get_cpu_usage(), get_mem_usage(), focused_title(), retile(), set_layout(name), window_count(), tilable_count()
let cpu = get_cpu_usage();
let mem = get_mem_usage();
let wins = tilable_count();
// In Rhai, string interpolation via `... ${var}` with backticks is not native — use + concat
"CPU " + cpu + "% MEM " + mem + "% [" + wins + "]"
