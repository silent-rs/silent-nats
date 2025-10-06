use thiserror::Error;

#[derive(Debug, Error)]
pub enum SubjectError {
    #[error("subject 不能为空")]
    Empty,
    #[error("subject 不能包含连续的句点或空 segment，第 {index} 段无效")]
    EmptyToken { index: usize },
    #[error("'>' 通配符必须作为最后一段出现")]
    TrailingWildcard,
    #[error("subject 段包含非法空白字符: {segment}")]
    InvalidWhitespace { segment: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Subject {
    raw: String,
    atoms: Vec<Atom>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Atom {
    Literal(String),
    One,
    Tail,
}

impl Subject {
    pub fn parse(input: &str) -> Result<Self, SubjectError> {
        if input.trim().is_empty() {
            return Err(SubjectError::Empty);
        }
        let mut atoms = Vec::new();
        let mut segments = input.split('.').enumerate();
        while let Some((index, segment)) = segments.next() {
            if segment.is_empty() {
                return Err(SubjectError::EmptyToken { index });
            }
            if segment.chars().any(|c| c.is_whitespace()) {
                return Err(SubjectError::InvalidWhitespace {
                    segment: segment.to_string(),
                });
            }
            match segment {
                "*" => atoms.push(Atom::One),
                ">" => {
                    if segments.next().is_some() {
                        return Err(SubjectError::TrailingWildcard);
                    }
                    atoms.push(Atom::Tail);
                    break;
                }
                _ => atoms.push(Atom::Literal(segment.to_string())),
            }
        }
        Ok(Self {
            raw: input.to_string(),
            atoms,
        })
    }

    pub fn raw(&self) -> &str {
        &self.raw
    }

    pub fn matches(&self, target: &str) -> bool {
        let target_tokens: Vec<&str> = target.split('.').collect();
        matches_tokens(&self.atoms, &target_tokens)
    }
}

fn matches_tokens(pattern: &[Atom], tokens: &[&str]) -> bool {
    if pattern.is_empty() {
        return tokens.is_empty();
    }
    match pattern[0] {
        Atom::Tail => true,
        Atom::One => {
            if tokens.is_empty() {
                return false;
            }
            matches_tokens(&pattern[1..], &tokens[1..])
        }
        Atom::Literal(ref expected) => {
            if let Some(actual) = tokens.first()
                && *actual == expected
            {
                return matches_tokens(&pattern[1..], &tokens[1..]);
            }
            false
        }
    }
}
