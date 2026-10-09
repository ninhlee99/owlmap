//! Output language: which language Claude writes prose in, and the fixed
//! labels OwlMap itself puts into the generated Markdown.

use clap::ValueEnum;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum Lang {
    #[default]
    En,
    Vi,
    Ja,
}

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Vi => "vi",
            Lang::Ja => "ja",
        }
    }

    /// Appended to every prompt. Code names never get translated.
    pub fn rule(self) -> &'static str {
        match self {
            Lang::En => "Write all prose in English.",
            Lang::Vi => "Write all prose in Vietnamese. Keep file paths, class, function, route and command names, \
                         and JSON keys exactly as they are; do not translate code or identifiers.",
            Lang::Ja => "Write all prose in Japanese. Keep file paths, class, function, route and command names, \
                         and JSON keys exactly as they are; do not translate code or identifiers.",
        }
    }

    pub fn labels(self) -> &'static Labels {
        match self {
            Lang::En => &EN,
            Lang::Vi => &VI,
            Lang::Ja => &JA,
        }
    }
}

pub struct Labels {
    pub key_files: &'static str,
    pub public_interface: &'static str,
    pub depends_on: &'static str,
    pub used_by: &'static str,
    pub data: &'static str,
    pub risks: &'static str,
    pub notes: &'static str,
    pub all_files: &'static str,
    pub module_failed: &'static str,
    pub doc_failed: &'static str,
    pub generated_for: &'static str,
    pub at_commit: &'static str,
    pub document: &'static str,
    pub answers: &'static str,
    pub architecture: (&'static str, &'static str),
    pub flows: (&'static str, &'static str),
    pub onboarding: (&'static str, &'static str),
    pub files_in: &'static str,
    pub modules: &'static str,
    pub module: &'static str,
    pub files: &'static str,
    pub purpose: &'static str,
    pub summary_failed: &'static str,
    pub footer: &'static str,
}

static EN: Labels = Labels {
    key_files: "Key files",
    public_interface: "Public interface",
    depends_on: "Depends on",
    used_by: "Used by",
    data: "Data",
    risks: "Handle with care",
    notes: "Notes",
    all_files: "All files in this module",
    module_failed: "OwlMap could not summarise this module",
    doc_failed: "OwlMap could not write this document",
    generated_for: "Generated documentation for",
    at_commit: "at commit",
    document: "Document",
    answers: "What it answers",
    architecture: ("Architecture", "How does the system fit together?"),
    flows: ("Key flows", "What happens when…?"),
    onboarding: ("Onboarding", "Where do I start?"),
    files_in: "files in",
    modules: "Modules",
    module: "Module",
    files: "Files",
    purpose: "Purpose",
    summary_failed: "_summary failed_",
    footer: "Written by OwlMap with Claude. Review before relying on it: statements marked \"Unverified:\" are inferences.",
};

static VI: Labels = Labels {
    key_files: "Tệp chính",
    public_interface: "Giao diện công khai",
    depends_on: "Phụ thuộc vào",
    used_by: "Được dùng bởi",
    data: "Dữ liệu",
    risks: "Cần cẩn thận",
    notes: "Ghi chú",
    all_files: "Tất cả tệp trong module này",
    module_failed: "OwlMap không tóm tắt được module này",
    doc_failed: "OwlMap không viết được tài liệu này",
    generated_for: "Tài liệu được tạo cho",
    at_commit: "tại commit",
    document: "Tài liệu",
    answers: "Trả lời câu hỏi",
    architecture: ("Kiến trúc", "Hệ thống ghép với nhau ra sao?"),
    flows: ("Các luồng chính", "Chuyện gì xảy ra khi…?"),
    onboarding: ("Onboarding", "Bắt đầu từ đâu?"),
    files_in: "tệp trong",
    modules: "Module",
    module: "Module",
    files: "Số tệp",
    purpose: "Mục đích",
    summary_failed: "_tóm tắt thất bại_",
    footer: "Do OwlMap viết cùng Claude. Hãy kiểm tra trước khi dựa vào: các ý ghi \"Unverified:\" là suy luận.",
};

static JA: Labels = Labels {
    key_files: "主要ファイル",
    public_interface: "公開インターフェース",
    depends_on: "依存先",
    used_by: "利用元",
    data: "データ",
    risks: "変更時の注意点",
    notes: "メモ",
    all_files: "このモジュールの全ファイル",
    module_failed: "OwlMap はこのモジュールを要約できませんでした",
    doc_failed: "OwlMap はこのドキュメントを作成できませんでした",
    generated_for: "対象",
    at_commit: "コミット",
    document: "ドキュメント",
    answers: "わかること",
    architecture: ("アーキテクチャ", "システム全体はどう構成されているか"),
    flows: ("主要フロー", "何が起きたら、どう動くか"),
    onboarding: ("オンボーディング", "どこから読めばよいか"),
    files_in: "ファイル /",
    modules: "モジュール",
    module: "モジュール",
    files: "ファイル数",
    purpose: "役割",
    summary_failed: "_要約に失敗_",
    footer: "OwlMap と Claude が作成しました。利用前に確認してください。「Unverified:」の記述は推測です。",
};
