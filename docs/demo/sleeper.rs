// A pane that stays alive and does nothing else.
//
// stage.sh copies this under each agent's own process name, because that name
// is what atc detects a pane by: state is only believed while the pane could
// still be running the agent that wrote it, so a demo pane sitting on `sleep`
// would be read as an ordinary job. A copy of a system binary will not run on
// macOS, so the demo builds its own.
fn main() {
    std::thread::sleep(std::time::Duration::from_secs(100_000));
}
