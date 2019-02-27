use failure::Error;

#[derive(PartialEq, Clone)]
pub enum OutputFormat {
    Json,
    JsonGz,
    JsonSnappy,
    JsonStream,
    JsonStreamGz,
    JsonStreamSnappy,
}

static ALL_EXTENTIONS: [(&str, OutputFormat); 6] = [
    (".json", OutputFormat::Json),
    (".jsonl", OutputFormat::JsonStream),
    (".json.gz", OutputFormat::JsonGz),
    (".jsonl.gz", OutputFormat::JsonStreamGz),
    (".json.snappy", OutputFormat::JsonSnappy),
    (".jsonl.snappy", OutputFormat::JsonStreamSnappy),
];

impl OutputFormat {
    pub fn from_filename(filename: &str) -> Result<OutputFormat, Error> {
        ALL_EXTENTIONS
            .iter()
            .find(|&&(ref e, _)| filename.ends_with(e))
            .map(|&(_, ref f)| f.clone())
            .ok_or_else(|| {
                let extensions_str = ALL_EXTENTIONS
                    .into_iter()
                    .map(|(e, _)| *e)
                    .collect::<Vec<_>>()
                    .join(", ");
                failure::err_msg(format!(
                    "Unable to detect the file format from filename '{}'. \
                     Accepted extensions are: {}",
                    filename, extensions_str
                ))
            })
    }
}
