use crate::lyrics::LyricLine;
#[derive(Clone, Debug, Default)]
pub struct LyricsContent {
    pub lines: Vec<LyricLine>,
    pub plain: String,
    pub quality: String,
    pub instrumental: bool,
}
