//! Theo dõi truy vấn XTGETTCAP bị chia nhỏ và trả các capability termul hỗ trợ.

#[derive(Debug, Default)]
pub(crate) struct XtgettcapTracker {
    state: State,
    body: Vec<u8>,
    pending: Vec<XtgettcapResponse>,
}

#[derive(Debug)]
pub(crate) struct XtgettcapResponse {
    pub(crate) end_offset: usize,
    pub(crate) bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, Default)]
enum State {
    #[default]
    Ground,
    Escape,
    DcsIntro,
    DcsPlus,
    Body,
    BodyEscape,
    Ignore,
    IgnoreEscape,
}

impl XtgettcapTracker {
    pub(crate) fn observe(&mut self, bytes: &[u8]) {
        for (index, &byte) in bytes.iter().enumerate() {
            match self.state {
                State::Ground => {
                    if byte == 0x1b {
                        self.state = State::Escape;
                    } else if byte == 0x90 {
                        self.body.clear();
                        self.state = State::DcsIntro;
                    }
                }
                State::Escape => match byte {
                    b'P' => {
                        self.body.clear();
                        self.state = State::DcsIntro;
                    }
                    0x1b => self.state = State::Escape,
                    _ => self.state = State::Ground,
                },
                State::DcsIntro => match byte {
                    b'+' => self.state = State::DcsPlus,
                    0x1b => self.state = State::IgnoreEscape,
                    0x9c => self.state = State::Ground,
                    _ => self.state = State::Ignore,
                },
                State::DcsPlus => match byte {
                    b'q' => self.state = State::Body,
                    0x1b => self.state = State::IgnoreEscape,
                    0x9c => self.state = State::Ground,
                    _ => self.state = State::Ignore,
                },
                State::Body => match byte {
                    0x1b => self.state = State::BodyEscape,
                    0x9c => {
                        self.finish(index + 1);
                        self.state = State::Ground;
                    }
                    _ if self.body.len() < 1024 => self.body.push(byte),
                    _ => {
                        self.body.clear();
                        self.state = State::Ignore;
                    }
                },
                State::BodyEscape => {
                    if byte == b'\\' {
                        self.finish(index + 1);
                        self.state = State::Ground;
                    } else if byte != 0x1b {
                        self.body.clear();
                        self.state = State::Ignore;
                    }
                }
                State::Ignore => {
                    if byte == 0x1b {
                        self.state = State::IgnoreEscape;
                    } else if byte == 0x9c {
                        self.state = State::Ground;
                    }
                }
                State::IgnoreEscape => {
                    if byte == b'\\' {
                        self.state = State::Ground;
                    } else if byte != 0x1b {
                        self.state = State::Ignore;
                    }
                }
            }
        }
    }

    fn finish(&mut self, end_offset: usize) {
        for capability in self.body.split(|byte| *byte == b';') {
            if let Some(response) = response_for(capability) {
                self.pending.push(XtgettcapResponse {
                    end_offset,
                    bytes: response,
                });
            }
        }
        self.body.clear();
    }

    pub(crate) fn drain(&mut self) -> Vec<XtgettcapResponse> {
        std::mem::take(&mut self.pending)
    }
}

fn response_for(capability: &[u8]) -> Option<Vec<u8>> {
    if capability.is_empty() || !capability.len().is_multiple_of(2) {
        return None;
    }
    let mut name = Vec::with_capacity(capability.len());
    for byte in capability {
        if !byte.is_ascii_hexdigit() {
            return None;
        }
        name.push(byte.to_ascii_uppercase());
    }
    let value = match name.as_slice() {
        b"5463" => None, // Tc: boolean true-color capability
        b"524742" => Some(b"8".as_slice()),
        b"73657472676266" => Some(b"\\E[38:2:%p1%d:%p2%d:%p3%dm".as_slice()),
        b"73657472676262" => Some(b"\\E[48:2:%p1%d:%p2%d:%p3%dm".as_slice()),
        b"4D73" => Some(b"\\E]52;%p1%s;%p2%s\\007".as_slice()),
        b"5375" => None, // Su: underline styles
        b"536D756C78" => Some(b"\\E[4:%p1%dm".as_slice()),
        b"536574756C63" => {
            Some(b"\\E[58:2::%p1%{65536}%/%d:%p1%{256}%/%{255}%&%d:%p1%{255}%&%d%;m".as_slice())
        }
        _ => return None,
    };

    let mut response = Vec::new();
    response.extend_from_slice(b"\x1bP1+r");
    response.extend_from_slice(&name);
    if let Some(value) = value {
        response.push(b'=');
        append_hex(value, &mut response);
    }
    response.extend_from_slice(b"\x1b\\");
    Some(response)
}

fn append_hex(bytes: &[u8], output: &mut Vec<u8>) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in bytes {
        output.push(HEX[usize::from(byte >> 4)]);
        output.push(HEX[usize::from(byte & 0x0f)]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_split_and_multiple_queries() {
        let mut tracker = XtgettcapTracker::default();
        tracker.observe(b"\x1bP+q5463;52");
        assert!(tracker.drain().is_empty());
        tracker.observe(b"4742\x1b\\");
        let responses = tracker.drain();
        assert_eq!(responses.len(), 2);
        assert!(responses[0].bytes.starts_with(b"\x1bP1+r5463"));
        assert!(responses[1].bytes.starts_with(b"\x1bP1+r524742=38"));
    }

    #[test]
    fn ignores_unknown_and_malformed_capabilities() {
        let mut tracker = XtgettcapTracker::default();
        tracker.observe(b"\x1bP+q123;zz\x1b\\");
        assert!(tracker.drain().is_empty());
    }
}
