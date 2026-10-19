use springai::{Unit, Vec3};

/// Move a unit using Zero-K's raw move command (more reliable than the
/// engine's `CMD_MOVE` with ZK's movement gadgets). The order expires 30s
/// after `frame` if not started.
pub fn kgb_move_to(unit: Unit, pos: Vec3, opts: i16, frame: i32) {
    unit.execute_custom_command(
        crate::defs::cmd::RAW_MOVE,
        &[pos.x, pos.y, pos.z],
        springai::opt::RIGHT_MOUSE_KEY | opts,
        frame + 30 * 30,
    );
}
