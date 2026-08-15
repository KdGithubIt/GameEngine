//! Typed animation parameters and deterministic one-dimensional blend sampling.
//!
//! Runtime values, persisted declarations, and Blend1D definitions share one
//! validated model so the GUI editor and project gameplay cannot disagree about
//! parameter kinds or threshold ordering.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

/// Current persisted schema version for [`AnimationMotionLibrary`].
pub const ANIMATION_MOTION_SCHEMA_VERSION: u32 = 1;

/// Runtime value of one named animation parameter.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum AnimationParameterValue {
    /// Persistent boolean value.
    Bool(bool),
    /// Persistent finite scalar value.
    Float(f32),
    /// One-shot flag consumed by a matching transition.
    Trigger(bool),
}

/// Stable declared kind of an animation parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnimationParameterKind {
    /// Persistent boolean value.
    Bool,
    /// Persistent finite scalar value.
    Float,
    /// One-shot transition trigger.
    Trigger,
}

impl AnimationParameterValue {
    /// Returns this value's declared kind.
    pub fn kind(self) -> AnimationParameterKind {
        match self {
            Self::Bool(_) => AnimationParameterKind::Bool,
            Self::Float(_) => AnimationParameterKind::Float,
            Self::Trigger(_) => AnimationParameterKind::Trigger,
        }
    }
}

/// Type mismatch or invalid scalar supplied to [`AnimationParameters`].
#[derive(Debug, Clone, PartialEq)]
pub enum AnimationParameterError {
    /// Parameter names must not be empty or whitespace-only.
    BlankName,
    /// The caller requested a different parameter type than the stored type.
    TypeMismatch {
        /// Stable parameter name.
        name: String,
        /// Stored parameter kind.
        stored: &'static str,
        /// Requested parameter kind.
        requested: &'static str,
    },
    /// Float parameters reject NaN and infinities.
    NonFiniteFloat {
        /// Stable parameter name.
        name: String,
        /// Rejected value.
        value: f32,
    },
}

impl fmt::Display for AnimationParameterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BlankName => write!(formatter, "animation parameter name must not be blank"),
            Self::TypeMismatch {
                name,
                stored,
                requested,
            } => write!(
                formatter,
                "animation parameter `{name}` is {stored}, not {requested}"
            ),
            Self::NonFiniteFloat { name, value } => write!(
                formatter,
                "animation float parameter `{name}` must be finite, found {value}"
            ),
        }
    }
}

impl std::error::Error for AnimationParameterError {}

/// Deterministic name-keyed animation parameter table.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AnimationParameters {
    values: BTreeMap<String, AnimationParameterValue>,
}

impl AnimationParameters {
    /// Creates an empty parameter table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Iterates parameter names and values in deterministic name order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, AnimationParameterValue)> {
        self.values
            .iter()
            .map(|(name, value)| (name.as_str(), *value))
    }

    /// Sets or creates a persistent boolean parameter.
    ///
    /// # Errors
    ///
    /// Returns [`AnimationParameterError::TypeMismatch`] when `name` already
    /// belongs to a float or trigger parameter.
    pub fn set_bool(
        &mut self,
        name: impl Into<String>,
        value: bool,
    ) -> Result<(), AnimationParameterError> {
        self.set_typed(name, AnimationParameterValue::Bool(value), "bool")
    }

    /// Reads a boolean parameter. Missing parameters read `false`.
    ///
    /// # Errors
    ///
    /// Returns [`AnimationParameterError::TypeMismatch`] when the stored value
    /// is not a boolean.
    pub fn bool(&self, name: &str) -> Result<bool, AnimationParameterError> {
        match self.values.get(name) {
            Some(AnimationParameterValue::Bool(value)) => Ok(*value),
            Some(value) => Err(type_mismatch(name, value, "bool")),
            None => Ok(false),
        }
    }

    /// Sets or creates a persistent finite float parameter.
    ///
    /// # Errors
    ///
    /// Returns an error for NaN, infinities, or an existing non-float parameter.
    pub fn set_float(
        &mut self,
        name: impl Into<String>,
        value: f32,
    ) -> Result<(), AnimationParameterError> {
        let name = validated_name(name)?;
        if !value.is_finite() {
            return Err(AnimationParameterError::NonFiniteFloat { name, value });
        }
        self.set_typed(name, AnimationParameterValue::Float(value), "float")
    }

    /// Reads a float parameter. Missing parameters read `0.0`.
    ///
    /// # Errors
    ///
    /// Returns [`AnimationParameterError::TypeMismatch`] when the stored value
    /// is not a float.
    pub fn float(&self, name: &str) -> Result<f32, AnimationParameterError> {
        match self.values.get(name) {
            Some(AnimationParameterValue::Float(value)) => Ok(*value),
            Some(value) => Err(type_mismatch(name, value, "float")),
            None => Ok(0.0),
        }
    }

    /// Sets a one-shot trigger which remains pending until consumed.
    ///
    /// # Errors
    ///
    /// Returns [`AnimationParameterError::TypeMismatch`] when `name` already
    /// belongs to a boolean or float parameter.
    pub fn trigger(&mut self, name: impl Into<String>) -> Result<(), AnimationParameterError> {
        self.set_typed(name, AnimationParameterValue::Trigger(true), "trigger")
    }

    /// Returns whether a trigger is pending without consuming it.
    ///
    /// # Errors
    ///
    /// Returns [`AnimationParameterError::TypeMismatch`] when the stored value
    /// is not a trigger.
    pub fn trigger_pending(&self, name: &str) -> Result<bool, AnimationParameterError> {
        match self.values.get(name) {
            Some(AnimationParameterValue::Trigger(value)) => Ok(*value),
            Some(value) => Err(type_mismatch(name, value, "trigger")),
            None => Ok(false),
        }
    }

    /// Consumes a pending trigger and returns whether it was set.
    ///
    /// # Errors
    ///
    /// Returns [`AnimationParameterError::TypeMismatch`] when the stored value
    /// is not a trigger.
    pub fn consume_trigger(&mut self, name: &str) -> Result<bool, AnimationParameterError> {
        match self.values.get_mut(name) {
            Some(AnimationParameterValue::Trigger(value)) => Ok(std::mem::take(value)),
            Some(value) => Err(type_mismatch(name, value, "trigger")),
            None => Ok(false),
        }
    }

    fn set_typed(
        &mut self,
        name: impl Into<String>,
        value: AnimationParameterValue,
        requested: &'static str,
    ) -> Result<(), AnimationParameterError> {
        let name = validated_name(name)?;
        if let Some(stored) = self.values.get(&name)
            && parameter_kind(stored) != requested {
                return Err(type_mismatch(&name, stored, requested));
            }
        self.values.insert(name, value);
        Ok(())
    }
}

fn validated_name(name: impl Into<String>) -> Result<String, AnimationParameterError> {
    let name = name.into();
    if name.trim().is_empty() {
        Err(AnimationParameterError::BlankName)
    } else {
        Ok(name)
    }
}

fn type_mismatch(
    name: &str,
    stored: &AnimationParameterValue,
    requested: &'static str,
) -> AnimationParameterError {
    AnimationParameterError::TypeMismatch {
        name: name.to_owned(),
        stored: parameter_kind(stored),
        requested,
    }
}

fn parameter_kind(value: &AnimationParameterValue) -> &'static str {
    match value {
        AnimationParameterValue::Bool(_) => "bool",
        AnimationParameterValue::Float(_) => "float",
        AnimationParameterValue::Trigger(_) => "trigger",
    }
}

/// One threshold and motion key in a one-dimensional blend definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Blend1dPoint {
    /// Scalar position at which this motion receives full weight.
    pub threshold: f32,
    /// Motion slot or project-owned motion identifier.
    pub motion: String,
}

/// Validation failure for a one-dimensional blend definition.
#[derive(Debug, Clone, PartialEq)]
pub enum Blend1dError {
    /// At least one point is required.
    Empty,
    /// Every threshold must be finite.
    NonFiniteThreshold {
        /// Rejected point index after sorting input order is preserved for diagnostics.
        index: usize,
        /// Rejected threshold.
        value: f32,
    },
    /// Motion identifiers must not be blank.
    BlankMotion {
        /// Rejected point index.
        index: usize,
    },
    /// Two points may not occupy the same threshold.
    DuplicateThreshold(f32),
}

impl fmt::Display for Blend1dError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(formatter, "Blend1D requires at least one point"),
            Self::NonFiniteThreshold { index, value } => write!(
                formatter,
                "Blend1D point {index} threshold must be finite, found {value}"
            ),
            Self::BlankMotion { index } => {
                write!(formatter, "Blend1D point {index} motion must not be blank")
            }
            Self::DuplicateThreshold(value) => {
                write!(formatter, "Blend1D threshold {value} is duplicated")
            }
        }
    }
}

impl std::error::Error for Blend1dError {}

/// Validated ordered points for one-dimensional locomotion blending.
#[derive(Debug, Clone, PartialEq)]
pub struct Blend1d {
    points: Vec<Blend1dPoint>,
}

impl Blend1d {
    /// Validates and sorts points by ascending threshold.
    ///
    /// # Errors
    ///
    /// Returns [`Blend1dError`] for an empty definition, invalid thresholds,
    /// blank motion IDs, or duplicate thresholds.
    pub fn new(mut points: Vec<Blend1dPoint>) -> Result<Self, Blend1dError> {
        if points.is_empty() {
            return Err(Blend1dError::Empty);
        }
        for (index, point) in points.iter().enumerate() {
            if !point.threshold.is_finite() {
                return Err(Blend1dError::NonFiniteThreshold {
                    index,
                    value: point.threshold,
                });
            }
            if point.motion.trim().is_empty() {
                return Err(Blend1dError::BlankMotion { index });
            }
        }
        points.sort_by(|left, right| left.threshold.total_cmp(&right.threshold));
        for pair in points.windows(2) {
            if pair[0].threshold == pair[1].threshold {
                return Err(Blend1dError::DuplicateThreshold(pair[0].threshold));
            }
        }
        Ok(Self { points })
    }

    /// Returns validated points in ascending threshold order.
    pub fn points(&self) -> &[Blend1dPoint] {
        &self.points
    }

    /// Samples at most two neighbouring motions for `value`.
    ///
    /// Values below or above the authored range clamp to the nearest endpoint.
    pub fn sample(&self, value: f32) -> Blend1dSample<'_> {
        if self.points.len() == 1 || !value.is_finite() || value <= self.points[0].threshold {
            return Blend1dSample::single(&self.points[0]);
        }
        let last = self
            .points
            .last()
            .expect("validated Blend1D always contains at least one point");
        if value >= last.threshold {
            return Blend1dSample::single(last);
        }
        let upper_index = self.points.partition_point(|point| point.threshold < value);
        let lower = &self.points[upper_index - 1];
        let upper = &self.points[upper_index];
        let upper_weight =
            ((value - lower.threshold) / (upper.threshold - lower.threshold)).clamp(0.0, 1.0);
        Blend1dSample {
            lower,
            upper: Some(upper),
            lower_weight: 1.0 - upper_weight,
            upper_weight,
        }
    }
}

/// Result of one [`Blend1d::sample`] call.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Blend1dSample<'a> {
    /// Lower or clamped endpoint motion.
    pub lower: &'a Blend1dPoint,
    /// Upper neighbouring motion, absent for a clamped endpoint.
    pub upper: Option<&'a Blend1dPoint>,
    /// Normalized weight for [`Self::lower`].
    pub lower_weight: f32,
    /// Normalized weight for [`Self::upper`], or zero when absent.
    pub upper_weight: f32,
}

impl<'a> Blend1dSample<'a> {
    fn single(point: &'a Blend1dPoint) -> Self {
        Self {
            lower: point,
            upper: None,
            lower_weight: 1.0,
            upper_weight: 0.0,
        }
    }
}

/// One persisted parameter declaration and its initial runtime value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnimationParameterDeclaration {
    /// Stable parameter name used by graphs and project gameplay.
    pub name: String,
    /// Initial value and stable parameter kind.
    pub default: AnimationParameterValue,
}

/// One named persisted Blend1D definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Blend1dDefinition {
    /// Stable blend identifier.
    pub id: String,
    /// Float parameter that drives sampling.
    pub parameter: String,
    /// Authored threshold and motion points.
    pub points: Vec<Blend1dPoint>,
}

impl Blend1dDefinition {
    /// Builds the validated runtime blend.
    ///
    /// # Errors
    ///
    /// Returns [`Blend1dError`] for malformed points.
    pub fn build(&self) -> Result<Blend1d, Blend1dError> {
        Blend1d::new(self.points.clone())
    }
}

/// Persisted parameter and Blend1D asset edited by the Motion Designer GUI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnimationMotionLibrary {
    /// File format version.
    pub schema_version: u32,
    /// Declared runtime parameters.
    #[serde(default)]
    pub parameters: Vec<AnimationParameterDeclaration>,
    /// Named one-dimensional blend definitions.
    #[serde(default)]
    pub blends: Vec<Blend1dDefinition>,
}

impl Default for AnimationMotionLibrary {
    fn default() -> Self {
        Self {
            schema_version: ANIMATION_MOTION_SCHEMA_VERSION,
            parameters: vec![AnimationParameterDeclaration {
                name: "speed".to_owned(),
                default: AnimationParameterValue::Float(0.0),
            }],
            blends: vec![Blend1dDefinition {
                id: "locomotion".to_owned(),
                parameter: "speed".to_owned(),
                points: vec![
                    Blend1dPoint {
                        threshold: 0.0,
                        motion: "idle".to_owned(),
                    },
                    Blend1dPoint {
                        threshold: 2.0,
                        motion: "walk".to_owned(),
                    },
                    Blend1dPoint {
                        threshold: 6.0,
                        motion: "run".to_owned(),
                    },
                ],
            }],
        }
    }
}

impl AnimationMotionLibrary {
    /// Parses and validates a motion library from JSON.
    ///
    /// # Errors
    ///
    /// Returns [`AnimationMotionLibraryError`] for JSON or validation failures.
    pub fn from_json_str(json: &str) -> Result<Self, AnimationMotionLibraryError> {
        let library: Self =
            serde_json::from_str(json).map_err(AnimationMotionLibraryError::Json)?;
        library.validate()?;
        Ok(library)
    }

    /// Serializes this library as pretty JSON after validation.
    ///
    /// # Errors
    ///
    /// Returns [`AnimationMotionLibraryError`] for validation or JSON failures.
    pub fn to_json_string(&self) -> Result<String, AnimationMotionLibraryError> {
        self.validate()?;
        serde_json::to_string_pretty(self).map_err(AnimationMotionLibraryError::Json)
    }

    /// Validates names, kinds, defaults, and every Blend1D definition.
    ///
    /// # Errors
    ///
    /// Returns the first deterministic validation failure.
    pub fn validate(&self) -> Result<(), AnimationMotionLibraryError> {
        if self.schema_version != ANIMATION_MOTION_SCHEMA_VERSION {
            return Err(AnimationMotionLibraryError::UnsupportedSchema {
                found: self.schema_version,
            });
        }
        let mut parameter_names = BTreeSet::new();
        let mut float_parameters = BTreeSet::new();
        for (index, parameter) in self.parameters.iter().enumerate() {
            if parameter.name.trim().is_empty() {
                return Err(AnimationMotionLibraryError::BlankParameter { index });
            }
            if !parameter_names.insert(parameter.name.as_str()) {
                return Err(AnimationMotionLibraryError::DuplicateParameter(
                    parameter.name.clone(),
                ));
            }
            match parameter.default {
                AnimationParameterValue::Float(value) => {
                    if !value.is_finite() {
                        return Err(AnimationMotionLibraryError::NonFiniteDefault {
                            parameter: parameter.name.clone(),
                            value,
                        });
                    }
                    float_parameters.insert(parameter.name.as_str());
                }
                AnimationParameterValue::Trigger(true) => {
                    return Err(AnimationMotionLibraryError::PendingTriggerDefault(
                        parameter.name.clone(),
                    ));
                }
                AnimationParameterValue::Bool(_) | AnimationParameterValue::Trigger(false) => {}
            }
        }
        let mut blend_ids = BTreeSet::new();
        for (index, blend) in self.blends.iter().enumerate() {
            if blend.id.trim().is_empty() {
                return Err(AnimationMotionLibraryError::BlankBlendId { index });
            }
            if !blend_ids.insert(blend.id.as_str()) {
                return Err(AnimationMotionLibraryError::DuplicateBlendId(
                    blend.id.clone(),
                ));
            }
            if !float_parameters.contains(blend.parameter.as_str()) {
                return Err(AnimationMotionLibraryError::InvalidBlendParameter {
                    blend: blend.id.clone(),
                    parameter: blend.parameter.clone(),
                });
            }
            blend
                .build()
                .map_err(|source| AnimationMotionLibraryError::InvalidBlend {
                    blend: blend.id.clone(),
                    source,
                })?;
        }
        Ok(())
    }

    /// Creates a runtime parameter table using every declared default.
    ///
    /// # Errors
    ///
    /// Returns [`AnimationMotionLibraryError`] when the library is invalid.
    pub fn instantiate_parameters(
        &self,
    ) -> Result<AnimationParameters, AnimationMotionLibraryError> {
        self.validate()?;
        let mut parameters = AnimationParameters::new();
        for declaration in &self.parameters {
            match declaration.default {
                AnimationParameterValue::Bool(value) => parameters
                    .set_bool(declaration.name.clone(), value)
                    .expect("validated declaration kind is stable"),
                AnimationParameterValue::Float(value) => parameters
                    .set_float(declaration.name.clone(), value)
                    .expect("validated float default is finite"),
                AnimationParameterValue::Trigger(false) => {
                    parameters.values.insert(
                        declaration.name.clone(),
                        AnimationParameterValue::Trigger(false),
                    );
                }
                AnimationParameterValue::Trigger(true) => unreachable!("validation rejects it"),
            }
        }
        Ok(parameters)
    }

    /// Builds a named runtime Blend1D definition.
    ///
    /// # Errors
    ///
    /// Returns [`AnimationMotionLibraryError`] when the library or blend is invalid.
    pub fn build_blend(&self, id: &str) -> Result<Option<Blend1d>, AnimationMotionLibraryError> {
        self.validate()?;
        self.blends
            .iter()
            .find(|blend| blend.id == id)
            .map(Blend1dDefinition::build)
            .transpose()
            .map_err(|source| AnimationMotionLibraryError::InvalidBlend {
                blend: id.to_owned(),
                source,
            })
    }
}

/// Loading, serialization, or validation failure for [`AnimationMotionLibrary`].
#[derive(Debug)]
pub enum AnimationMotionLibraryError {
    /// JSON parsing or serialization failed.
    Json(serde_json::Error),
    /// The schema version is unsupported.
    UnsupportedSchema {
        /// Rejected version.
        found: u32,
    },
    /// A parameter name is blank.
    BlankParameter {
        /// Zero-based declaration index.
        index: usize,
    },
    /// The same parameter name appears more than once.
    DuplicateParameter(String),
    /// A float default is NaN or infinity.
    NonFiniteDefault {
        /// Parameter name.
        parameter: String,
        /// Rejected value.
        value: f32,
    },
    /// Trigger defaults must start unconsumed.
    PendingTriggerDefault(String),
    /// A blend ID is blank.
    BlankBlendId {
        /// Zero-based blend index.
        index: usize,
    },
    /// The same blend ID appears more than once.
    DuplicateBlendId(String),
    /// A blend references a missing or non-float parameter.
    InvalidBlendParameter {
        /// Blend ID.
        blend: String,
        /// Rejected parameter name.
        parameter: String,
    },
    /// A blend contains invalid threshold data.
    InvalidBlend {
        /// Blend ID.
        blend: String,
        /// Blend validation failure.
        source: Blend1dError,
    },
}

impl fmt::Display for AnimationMotionLibraryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(formatter, "animation motion JSON error: {error}"),
            Self::UnsupportedSchema { found } => write!(
                formatter,
                "unsupported animation motion schema {found}; expected {ANIMATION_MOTION_SCHEMA_VERSION}"
            ),
            Self::BlankParameter { index } => {
                write!(formatter, "animation parameter {index} has a blank name")
            }
            Self::DuplicateParameter(name) => {
                write!(formatter, "animation parameter `{name}` is duplicated")
            }
            Self::NonFiniteDefault { parameter, value } => write!(
                formatter,
                "animation parameter `{parameter}` has non-finite default {value}"
            ),
            Self::PendingTriggerDefault(name) => write!(
                formatter,
                "animation trigger `{name}` must default to unconsumed"
            ),
            Self::BlankBlendId { index } => write!(formatter, "Blend1D {index} has a blank ID"),
            Self::DuplicateBlendId(id) => write!(formatter, "Blend1D ID `{id}` is duplicated"),
            Self::InvalidBlendParameter { blend, parameter } => write!(
                formatter,
                "Blend1D `{blend}` requires declared float parameter `{parameter}`"
            ),
            Self::InvalidBlend { blend, source } => {
                write!(formatter, "Blend1D `{blend}` is invalid: {source}")
            }
        }
    }
}

impl std::error::Error for AnimationMotionLibraryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            Self::InvalidBlend { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_is_consumed_once() {
        let mut parameters = AnimationParameters::new();
        parameters.trigger("attack").expect("trigger must be valid");

        assert!(parameters
            .consume_trigger("attack")
            .expect("trigger must be readable"));
        assert!(!parameters
            .consume_trigger("attack")
            .expect("consumed trigger remains valid"));
    }

    #[test]
    fn parameter_type_is_stable_after_first_write() {
        let mut parameters = AnimationParameters::new();
        parameters
            .set_float("speed", 1.0)
            .expect("float must be valid");

        let error = parameters
            .set_bool("speed", true)
            .expect_err("existing float must reject bool writes");

        assert!(matches!(
            error,
            AnimationParameterError::TypeMismatch {
                stored: "float",
                requested: "bool",
                ..
            }
        ));
    }

    #[test]
    fn blend_sample_interpolates_neighbouring_points() {
        let blend = Blend1d::new(vec![
            Blend1dPoint {
                threshold: 0.0,
                motion: "idle".to_owned(),
            },
            Blend1dPoint {
                threshold: 2.0,
                motion: "walk".to_owned(),
            },
            Blend1dPoint {
                threshold: 6.0,
                motion: "run".to_owned(),
            },
        ])
        .expect("blend definition must be valid");

        let sample = blend.sample(4.0);

        assert_eq!(sample.lower.motion, "walk");
        assert_eq!(sample.upper.map(|point| point.motion.as_str()), Some("run"));
        assert!((sample.lower_weight - 0.5).abs() < f32::EPSILON);
        assert!((sample.upper_weight - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn library_round_trip_builds_runtime_values() {
        let library = AnimationMotionLibrary::default();
        let json = library.to_json_string().expect("library must serialize");
        let loaded = AnimationMotionLibrary::from_json_str(&json).expect("library must load");
        let parameters = loaded
            .instantiate_parameters()
            .expect("defaults must instantiate");

        assert_eq!(parameters.float("speed").expect("speed is float"), 0.0);
        assert!(loaded
            .build_blend("locomotion")
            .expect("blend must build")
            .is_some());
    }
}
