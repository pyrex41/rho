use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum HashlineEdit {
    SetLine { set_line: SetLineOp },
    ReplaceLines { replace_lines: ReplaceLinesOp },
    InsertAfter { insert_after: InsertAfterOp },
    Replace { replace: ReplaceOp },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetLineOp {
    pub anchor: String,
    pub new_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplaceLinesOp {
    pub start_anchor: String,
    pub end_anchor: String,
    pub new_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsertAfterOp {
    pub anchor: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplaceOp {
    pub old_text: String,
    pub new_text: String,
    #[serde(default)]
    pub all: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_set_line() {
        let json = r#"{"set_line": {"anchor": "5:a3", "new_text": "hello"}}"#;
        let edit: HashlineEdit = serde_json::from_str(json).unwrap();
        match edit {
            HashlineEdit::SetLine { set_line } => {
                assert_eq!(set_line.anchor, "5:a3");
                assert_eq!(set_line.new_text, "hello");
            }
            _ => panic!("Expected SetLine variant"),
        }
    }

    #[test]
    fn test_deserialize_replace_lines() {
        let json = r#"{"replace_lines": {"start_anchor": "2:ab", "end_anchor": "5:cd", "new_text": "new content"}}"#;
        let edit: HashlineEdit = serde_json::from_str(json).unwrap();
        match edit {
            HashlineEdit::ReplaceLines { replace_lines } => {
                assert_eq!(replace_lines.start_anchor, "2:ab");
                assert_eq!(replace_lines.end_anchor, "5:cd");
                assert_eq!(replace_lines.new_text, "new content");
            }
            _ => panic!("Expected ReplaceLines variant"),
        }
    }

    #[test]
    fn test_deserialize_insert_after() {
        let json = r#"{"insert_after": {"anchor": "3:ef", "text": "inserted line"}}"#;
        let edit: HashlineEdit = serde_json::from_str(json).unwrap();
        match edit {
            HashlineEdit::InsertAfter { insert_after } => {
                assert_eq!(insert_after.anchor, "3:ef");
                assert_eq!(insert_after.text, "inserted line");
            }
            _ => panic!("Expected InsertAfter variant"),
        }
    }

    #[test]
    fn test_deserialize_replace() {
        let json = r#"{"replace": {"old_text": "old", "new_text": "new", "all": true}}"#;
        let edit: HashlineEdit = serde_json::from_str(json).unwrap();
        match edit {
            HashlineEdit::Replace { replace } => {
                assert_eq!(replace.old_text, "old");
                assert_eq!(replace.new_text, "new");
                assert!(replace.all);
            }
            _ => panic!("Expected Replace variant"),
        }
    }

    #[test]
    fn test_deserialize_replace_default_all() {
        let json = r#"{"replace": {"old_text": "old", "new_text": "new"}}"#;
        let edit: HashlineEdit = serde_json::from_str(json).unwrap();
        match edit {
            HashlineEdit::Replace { replace } => {
                assert!(!replace.all);
            }
            _ => panic!("Expected Replace variant"),
        }
    }

    #[test]
    fn test_serialize_roundtrip() {
        let edit = HashlineEdit::SetLine {
            set_line: SetLineOp {
                anchor: "1:ff".to_string(),
                new_text: "test".to_string(),
            },
        };
        let json = serde_json::to_string(&edit).unwrap();
        let back: HashlineEdit = serde_json::from_str(&json).unwrap();
        match back {
            HashlineEdit::SetLine { set_line } => {
                assert_eq!(set_line.anchor, "1:ff");
            }
            _ => panic!("Expected SetLine variant"),
        }
    }
}
