//! Single-source declarations for built-in authorable components.
//!
//! Before this module a built-in component repeated its field list in three
//! places: the authoring [`ComponentSchema`], the Inspector hint table, and
//! the bridge spawn callback that re-read and re-range-checked every field.
//! The three could drift, and adding one component meant editing all of them.
//!
//! A [`BuiltinComponent`] now declares each field exactly once as a
//! [`FieldDef`]. The schema, the Inspector metadata, and the bridge's
//! per-field validation are all derived from that one table, so a new
//! component is one entry plus its spawn callback.

use super::{AssetKind, InspectorFieldCondition, InspectorFieldControl, InspectorHint, SpawnFn};
use engine_authoring::id::ComponentTypeId;
use engine_authoring::schema::{ComponentSchema, FieldSchema, FieldType};
use engine_authoring::value::Value;

/// Const-constructible mirror of [`FieldType`].
///
/// [`FieldType::Array`] owns a `Box`, which cannot be built in a `const`
/// item, so declaration tables use this enum and convert on demand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    /// Declares [`FieldType::Bool`].
    Bool,
    /// Declares [`FieldType::I64`].
    I64,
    /// Declares [`FieldType::U64`].
    U64,
    /// Declares [`FieldType::F64`].
    F64,
    /// Declares [`FieldType::Vec2`].
    Vec2,
    /// Declares [`FieldType::Vec3`].
    Vec3,
    /// Declares [`FieldType::String`].
    Str,
    /// Declares [`FieldType::EntityRef`].
    EntityRef,
    /// Declares [`FieldType::AssetRef`].
    AssetRef,
    /// Declares [`FieldType::Object`].
    Object,
    /// Declares [`FieldType::Array`] of the referenced element kind.
    Array(&'static FieldKind),
}

impl FieldKind {
    /// Converts this declaration into the authoring-facing field type.
    pub fn to_field_type(self) -> FieldType {
        match self {
            Self::Bool => FieldType::Bool,
            Self::I64 => FieldType::I64,
            Self::U64 => FieldType::U64,
            Self::F64 => FieldType::F64,
            Self::Vec2 => FieldType::Vec2,
            Self::Vec3 => FieldType::Vec3,
            Self::Str => FieldType::String,
            Self::EntityRef => FieldType::EntityRef,
            Self::AssetRef => FieldType::AssetRef,
            Self::Object => FieldType::Object,
            Self::Array(element) => FieldType::Array(Box::new(element.to_field_type())),
        }
    }
}

/// How the default value of one declared field is produced.
///
/// Scalar defaults are written inline. Defaults that need an allocation, a
/// runtime type's `Default`, or a built-in asset ID use
/// [`Self::Computed`] so the whole declaration stays a `const` item.
#[derive(Debug, Clone, Copy)]
pub enum FieldDefaultSpec {
    /// The field has no default and starts unassigned.
    ///
    /// A required field declared this way is what ADR 0069 calls an
    /// unassigned reference: authoring keeps it editable and validation
    /// reports it as a non-blocking warning rather than a hard error.
    Unassigned,
    /// A literal boolean default.
    Bool(bool),
    /// A literal signed-integer default.
    I64(i64),
    /// A literal floating-point default.
    F64(f64),
    /// A literal string default.
    Str(&'static str),
    /// A default built at call time, for values no `const` item can hold.
    Computed(fn() -> Value),
}

/// Compares the values two specifications produce, not how they produce them.
///
/// Comparing [`FieldDefaultSpec::Computed`] by function pointer would be
/// meaningless: addresses are not guaranteed to be unique, and two providers
/// that yield the same default are the same default as far as authoring is
/// concerned.
impl PartialEq for FieldDefaultSpec {
    fn eq(&self, other: &Self) -> bool {
        self.to_value() == other.to_value()
    }
}

impl FieldDefaultSpec {
    /// Materializes the declared default, or `None` when unassigned.
    pub fn to_value(self) -> Option<Value> {
        match self {
            Self::Unassigned => None,
            Self::Bool(value) => Some(Value::Bool(value)),
            Self::I64(value) => Some(Value::I64(value)),
            Self::F64(value) => Some(Value::F64(value)),
            Self::Str(value) => Some(Value::String(value.to_owned())),
            Self::Computed(build) => Some(build()),
        }
    }
}

/// One authorable field of a built-in component.
///
/// This is the single source of truth for that field: the authoring schema,
/// the Inspector control, and the bridge's value validation are all derived
/// from it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FieldDef {
    /// The Rust-style field name as persisted in scene files.
    pub name: &'static str,
    /// Human-readable label shown by editing tools.
    pub display_name: &'static str,
    /// Description of the field's meaning and constraints.
    pub description: &'static str,
    /// Declared value type.
    pub kind: FieldKind,
    /// Whether the component is invalid without this field.
    pub required: bool,
    /// How the default value is produced.
    pub default: FieldDefaultSpec,
    /// Specialized editor control replacing the generic primitive editor.
    ///
    /// [`InspectorFieldControl::Number`] additionally declares the accepted
    /// range, which pre-Play validation and the scene bridge both enforce, so
    /// a bound is never written twice.
    pub control: Option<InspectorFieldControl>,
    /// Optional sibling-field visibility condition.
    pub visible_when: Option<InspectorFieldCondition>,
}

impl FieldDef {
    /// Declares a required field with no specialized control.
    pub const fn new(
        name: &'static str,
        display_name: &'static str,
        description: &'static str,
        kind: FieldKind,
        default: FieldDefaultSpec,
    ) -> Self {
        Self {
            name,
            display_name,
            description,
            kind,
            required: true,
            default,
            control: None,
            visible_when: None,
        }
    }

    /// Attaches a specialized Inspector control to this field.
    pub const fn with_control(mut self, control: InspectorFieldControl) -> Self {
        self.control = Some(control);
        self
    }

    /// Attaches a sibling-field visibility condition to this field.
    pub const fn when(mut self, condition: InspectorFieldCondition) -> Self {
        self.visible_when = Some(condition);
        self
    }

    /// Marks this field as optional for schema validation.
    pub const fn optional(mut self) -> Self {
        self.required = false;
        self
    }

    /// Builds the authoring schema entry for this field.
    pub fn to_field_schema(&self) -> FieldSchema {
        FieldSchema {
            name: self.name.to_owned(),
            display_name: self.display_name.to_owned(),
            description: self.description.to_owned(),
            field_type: self.kind.to_field_type(),
            required: self.required,
            default_value: self.default.to_value(),
        }
    }
}

/// Complete declaration of one built-in authorable component.
///
/// `builtin_components()` returns these in registration order; the registry,
/// every schema, and every Inspector hint are derived from that one list, so
/// the set of built-ins is never stated twice.
pub struct BuiltinComponent {
    /// Stable `engine.*` component type ID persisted in scene files.
    pub type_id: &'static str,
    /// Human-readable component name shown in pickers.
    pub display_name: &'static str,
    /// Description of the component's purpose for tools and agents.
    pub description: &'static str,
    /// Grouping category used by component pickers.
    pub category: &'static str,
    /// Schema version; increment when field definitions change.
    pub version: u32,
    /// Declared fields, in schema order.
    pub fields: &'static [FieldDef],
    /// Complete default value for components whose value is not an object.
    ///
    /// Asset-reference components persist a bare `asset_ref` rather than an
    /// object, so they declare no fields and set this instead.
    pub component_default: Option<fn() -> Value>,
    /// Asset category when the whole component value is an asset reference.
    ///
    /// `Some` produces [`InspectorHint::AssetRef`] instead of the
    /// field-driven hint.
    pub asset_component: Option<AssetKind>,
    /// Whether editing tools should use generic editors for every field.
    ///
    /// Components whose fields carry no specialized control set this so the
    /// registry reports [`InspectorHint::Default`], preserving the existing
    /// Inspector behavior for plain data components.
    pub generic_inspector: bool,
    /// Authoring-to-runtime spawn callback.
    pub spawn: SpawnFn,
    /// Schema supplied by the authoring crate instead of the field table.
    ///
    /// `engine.transform` and `engine.player_marker` are owned by the
    /// authoring built-in registry, which is the format authority for them.
    /// Restating their fields here would create exactly the second source of
    /// truth this module exists to remove.
    pub schema_override: Option<fn() -> ComponentSchema>,
    /// Whether editing tools should present this component collapsed until
    /// the author opens it.
    ///
    /// Set for components whose editor body is long enough to push the rest
    /// of an entity's components off screen: many fields, or a list whose
    /// length comes from project data rather than from this declaration.
    pub default_collapsed: bool,
}

impl BuiltinComponent {
    /// Declares a component whose value is an object built from `fields`.
    pub const fn new(
        type_id: &'static str,
        display_name: &'static str,
        description: &'static str,
        category: &'static str,
        version: u32,
        fields: &'static [FieldDef],
        spawn: SpawnFn,
    ) -> Self {
        Self {
            type_id,
            display_name,
            description,
            category,
            version,
            fields,
            component_default: None,
            asset_component: None,
            generic_inspector: false,
            spawn,
            schema_override: None,
            default_collapsed: false,
        }
    }

    /// Marks this component as edited with generic per-field editors.
    pub const fn generic_inspector(mut self) -> Self {
        self.generic_inspector = true;
        self
    }

    /// Marks this component as presented collapsed until the author opens it.
    pub const fn collapsed_by_default(mut self) -> Self {
        self.default_collapsed = true;
        self
    }

    /// Declares a component whose whole value is one asset reference.
    pub const fn asset_value(mut self, kind: AssetKind, default: fn() -> Value) -> Self {
        self.asset_component = Some(kind);
        self.component_default = Some(default);
        self
    }

    /// Takes the schema from the authoring built-in registry instead.
    pub const fn schema_from(mut self, build: fn() -> ComponentSchema) -> Self {
        self.schema_override = Some(build);
        self
    }

    /// Builds the authoring schema declared by this component.
    pub fn schema(&self) -> ComponentSchema {
        if let Some(build) = self.schema_override {
            return build();
        }
        ComponentSchema {
            type_id: ComponentTypeId::new(self.type_id),
            display_name: self.display_name.to_owned(),
            description: self.description.to_owned(),
            category: self.category.to_owned(),
            version: self.version,
            fields: self.fields.iter().map(FieldDef::to_field_schema).collect(),
            component_default: self.component_default.map(|build| build()),
        }
    }

    /// Builds the Inspector metadata declared by this component.
    pub fn inspector(&self) -> InspectorHint {
        match self.asset_component {
            Some(kind) => InspectorHint::AssetRef { kind },
            None if self.generic_inspector => InspectorHint::Default,
            None => InspectorHint::Fields {
                fields: self.fields,
            },
        }
    }

    /// Returns the declaration of one field by its persisted name.
    pub fn field(&self, name: &str) -> Option<&'static FieldDef> {
        self.fields.iter().find(|field| field.name == name)
    }
}

/// Declares a required `f64` field with an accepted numeric range.
pub const fn number(
    name: &'static str,
    display_name: &'static str,
    description: &'static str,
    default: f64,
    range: super::NumericRange,
) -> FieldDef {
    FieldDef::new(
        name,
        display_name,
        description,
        FieldKind::F64,
        FieldDefaultSpec::F64(default),
    )
    .with_control(InspectorFieldControl::Number(range))
}

/// Declares a required `f64` field with no declared bounds.
pub const fn float(
    name: &'static str,
    display_name: &'static str,
    description: &'static str,
    default: f64,
) -> FieldDef {
    FieldDef::new(
        name,
        display_name,
        description,
        FieldKind::F64,
        FieldDefaultSpec::F64(default),
    )
}

/// Declares a required boolean field.
pub const fn boolean(
    name: &'static str,
    display_name: &'static str,
    description: &'static str,
    default: bool,
) -> FieldDef {
    FieldDef::new(
        name,
        display_name,
        description,
        FieldKind::Bool,
        FieldDefaultSpec::Bool(default),
    )
}

/// Declares a required signed-integer field with an accepted range.
pub const fn integer(
    name: &'static str,
    display_name: &'static str,
    description: &'static str,
    default: i64,
    range: super::NumericRange,
) -> FieldDef {
    FieldDef::new(
        name,
        display_name,
        description,
        FieldKind::I64,
        FieldDefaultSpec::I64(default),
    )
    .with_control(InspectorFieldControl::Number(range))
}

/// Declares a required string field.
pub const fn text(
    name: &'static str,
    display_name: &'static str,
    description: &'static str,
    default: &'static str,
) -> FieldDef {
    FieldDef::new(
        name,
        display_name,
        description,
        FieldKind::Str,
        FieldDefaultSpec::Str(default),
    )
}

/// Declares a required string field edited as a fixed set of choices.
pub const fn enumeration(
    name: &'static str,
    display_name: &'static str,
    description: &'static str,
    default: &'static str,
    options: &'static [&'static str],
) -> FieldDef {
    text(name, display_name, description, default)
        .with_control(InspectorFieldControl::Enum(options))
}

/// Declares a required entity reference that starts unassigned.
///
/// Any scene entity is accepted, so the field uses the generic entity editor
/// rather than a filtered picker.
pub const fn entity_ref(
    name: &'static str,
    display_name: &'static str,
    description: &'static str,
) -> FieldDef {
    FieldDef::new(
        name,
        display_name,
        description,
        FieldKind::EntityRef,
        FieldDefaultSpec::Unassigned,
    )
}

/// Declares an entity reference restricted to entities carrying `required`.
///
/// The Inspector offers only matching entities and validation reports a
/// stored reference to a non-matching entity (ADR 0087 §4).
pub const fn filtered_entity_ref(
    name: &'static str,
    display_name: &'static str,
    description: &'static str,
    required: &'static [&'static str],
) -> FieldDef {
    entity_ref(name, display_name, description).with_control(InspectorFieldControl::EntityRef(
        required,
    ))
}

/// Declares a required asset reference filtered to one category.
///
/// `default` is `None` for references an author must assign, matching
/// ADR 0069's unassigned-reference state.
pub const fn asset_ref(
    name: &'static str,
    display_name: &'static str,
    description: &'static str,
    kind: AssetKind,
    default: FieldDefaultSpec,
) -> FieldDef {
    FieldDef::new(
        name,
        display_name,
        description,
        FieldKind::AssetRef,
        default,
    )
    .with_control(InspectorFieldControl::AssetRef(kind))
}
