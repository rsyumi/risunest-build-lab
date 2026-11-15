use crate::budget::{Budget, Error, OwnedString, OwnedVec};
use crate::text::{Text, TextData};

pub struct Utf8Decoder { decoder: encoding_rs::Decoder, finished: bool }
impl Default for Utf8Decoder {
    fn default() -> Self { Self::new() }
}
impl Utf8Decoder {
    pub fn new() -> Self { Self { decoder: encoding_rs::UTF_8.new_decoder_with_bom_removal(), finished: false } }
    pub fn decode(&mut self, input: &[u8], last: bool, budget: &Budget) -> Result<Text, Error> {
        let result = self.decode_inner(input, last, budget);
        if last || result.is_err() { self.finished = true; }
        result
    }
    fn decode_inner(&mut self, input: &[u8], last: bool, budget: &Budget) -> Result<Text, Error> {
        budget.work(input.len())?;
        if self.finished { return Err(Error::DecoderFinished); }
        let mut output = OwnedVec::new(budget)?;
        let mut position = 0;
        loop {
            budget.check()?;
            let mut block = [0u8; 4096];
            let (result, read, written, _) = self.decoder.decode_to_utf8(&input[position..], &mut block, last);
            position += read;
            if output.data.len().checked_add(written).filter(|length| *length <= budget.0.limits.max_json_bytes).is_none() {
                return Err(Error::Limit("decoded-text-bytes"));
            }
            output.extend(&block[..written])?;
            if result == encoding_rs::CoderResult::InputEmpty { break; }
        }
        self.finished = last;
        let text = OwnedString::from_utf8(output)?;
        budget.work(text.data.len())?;
        let units = text.data.encode_utf16().count();
        if units > budget.0.limits.max_text_units { return Err(Error::Limit("text-units")); }
        Ok(Text { data: TextData::Utf8(text), units })
    }
}
