//! Reading and writing one nested component value through a property path.

use crate::ui::*;

pub(in crate::ui) fn prepend_property_segment(
    edit: ComponentEdit,
    segment: PropertyPathSegment,
) -> ComponentEdit {
    match edit {
        ComponentEdit::Property { mut path, value } => {
            path.insert(0, segment);
            ComponentEdit::Property { path, value }
        }
        ComponentEdit::DraftProperty { mut path, value } => {
            path.insert(0, segment);
            ComponentEdit::DraftProperty { path, value }
        }
        ComponentEdit::CommitDraft { mut path } => {
            path.insert(0, segment);
            ComponentEdit::CommitDraft { path }
        }
        ComponentEdit::Whole(value) => ComponentEdit::Whole(value),
    }
}

pub(in crate::ui) fn upsert_property_value(
    value: &mut Value,
    path: &[PropertyPathSegment],
    replacement: Value,
) -> bool {
    let Some((first, rest)) = path.split_first() else {
        *value = replacement;
        return true;
    };

    match first {
        PropertyPathSegment::Field { name } => match value {
            Value::Object(fields) if rest.is_empty() => {
                fields.insert(name.clone(), replacement);
                true
            }
            Value::Object(fields) => {
                let Some(child) = fields.get_mut(name) else {
                    return false;
                };
                upsert_property_value(child, rest, replacement)
            }
            _ => false,
        },
        PropertyPathSegment::Index { index } => match value {
            Value::Array(values) => {
                let Some(child) = values.get_mut(*index) else {
                    return false;
                };
                upsert_property_value(child, rest, replacement)
            }
            _ => false,
        },
    }
}

pub(in crate::ui) fn property_value<'a>(
    value: &'a Value,
    path: &[PropertyPathSegment],
) -> Option<&'a Value> {
    let Some((first, rest)) = path.split_first() else {
        return Some(value);
    };

    match first {
        PropertyPathSegment::Field { name } => match value {
            Value::Object(fields) => property_value(fields.get(name)?, rest),
            _ => None,
        },
        PropertyPathSegment::Index { index } => match value {
            Value::Array(values) => property_value(values.get(*index)?, rest),
            _ => None,
        },
    }
}
