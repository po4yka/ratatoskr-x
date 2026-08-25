//! Granted-versus-requested scope auditing.

/// Returns every requested scope the grant omitted, as a multiset difference:
/// duplicates count per occurrence and the requested order is preserved.
#[must_use]
pub fn missing_scopes(requested: &[String], granted: &[String]) -> Vec<String> {
    let mut remaining = granted.to_vec();
    let mut missing = Vec::new();
    for scope in requested {
        match remaining
            .iter()
            .position(|granted_scope| granted_scope == scope)
        {
            Some(position) => {
                drop(remaining.remove(position));
            }
            None => missing.push(scope.clone()),
        }
    }
    missing
}
