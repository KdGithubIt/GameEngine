//! Platform-independent physical keyboard and mouse-button identities.

/// Physical keyboard keys used by persisted bindings and virtual input.
///
/// Platform adapters translate their native key identifiers into this stable
/// engine-owned vocabulary when a backend-neutral contract is required.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    /// Physical A key.
    KeyA,
    /// Physical B key.
    KeyB,
    /// Physical C key.
    KeyC,
    /// Physical D key.
    KeyD,
    /// Physical E key.
    KeyE,
    /// Physical F key.
    KeyF,
    /// Physical G key.
    KeyG,
    /// Physical H key.
    KeyH,
    /// Physical I key.
    KeyI,
    /// Physical J key.
    KeyJ,
    /// Physical K key.
    KeyK,
    /// Physical L key.
    KeyL,
    /// Physical M key.
    KeyM,
    /// Physical N key.
    KeyN,
    /// Physical O key.
    KeyO,
    /// Physical P key.
    KeyP,
    /// Physical Q key.
    KeyQ,
    /// Physical R key.
    KeyR,
    /// Physical S key.
    KeyS,
    /// Physical T key.
    KeyT,
    /// Physical U key.
    KeyU,
    /// Physical V key.
    KeyV,
    /// Physical W key.
    KeyW,
    /// Physical X key.
    KeyX,
    /// Physical Y key.
    KeyY,
    /// Physical Z key.
    KeyZ,
    /// Physical 0 digit key.
    Digit0,
    /// Physical 1 digit key.
    Digit1,
    /// Physical 2 digit key.
    Digit2,
    /// Physical 3 digit key.
    Digit3,
    /// Physical 4 digit key.
    Digit4,
    /// Physical 5 digit key.
    Digit5,
    /// Physical 6 digit key.
    Digit6,
    /// Physical 7 digit key.
    Digit7,
    /// Physical 8 digit key.
    Digit8,
    /// Physical 9 digit key.
    Digit9,
    /// Up-arrow key.
    ArrowUp,
    /// Down-arrow key.
    ArrowDown,
    /// Left-arrow key.
    ArrowLeft,
    /// Right-arrow key.
    ArrowRight,
    /// Space key.
    Space,
    /// Enter key.
    Enter,
    /// Escape key.
    Escape,
    /// Tab key.
    Tab,
    /// Left Shift key.
    ShiftLeft,
    /// Right Shift key.
    ShiftRight,
    /// Left Control key.
    ControlLeft,
    /// Right Control key.
    ControlRight,
}

/// Mouse buttons used by persisted bindings and virtual input.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    /// Primary mouse button.
    Left,
    /// Secondary mouse button.
    Right,
    /// Middle mouse button.
    Middle,
    /// Browser-style back button.
    Back,
    /// Browser-style forward button.
    Forward,
}
