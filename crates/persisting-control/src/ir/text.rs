use super::*;
use std::{fmt, str::FromStr};
fn json(f: &mut fmt::Formatter<'_>, value: &impl Serialize) -> fmt::Result {
    f.write_str(&serde_json::to_string(value).map_err(|_| fmt::Error)?)
}
impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bytes(bytes) => {
                write!(f, "bytes(")?;
                json(f, bytes)?;
                write!(f, ")")
            }
            Self::U64(value) => write!(f, "{value}"),
        }
    }
}
impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Success { value } => write!(f, "ok({value})"),
            Self::Error { failure } => {
                write!(f, "error(")?;
                json(f, failure)?;
                write!(f, ")")
            }
        }
    }
}
impl fmt::Display for Expression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}(", self.operation.code().name())?;
        json(f, &self.operation.file())?;
        match &self.operation {
            Operation::Read { offset, length, .. } => {
                write!(f, ", offset: {offset}, length: {length})")?
            }
            Operation::Write { offset, data, .. } => {
                write!(f, ", offset: {offset}, data: bytes(")?;
                json(f, data)?;
                write!(f, "))")?;
            }
        }
        for layer in &self.contexts {
            write!(f, " |> ")?;
            let (name, argument) = match layer {
                Layer::Vm { name } => ("vm", name),
                Layer::Remote { name } => ("remote", name),
                Layer::Overlay { name } => ("overlay", name),
                Layer::Deny { reason } => ("deny", reason),
                Layer::Mock { value } => {
                    write!(f, "mock({value})")?;
                    continue;
                }
            };
            write!(f, "{name}(")?;
            json(f, argument)?;
            write!(f, ")")?;
        }
        Ok(())
    }
}
impl FromStr for Expression {
    type Err = anyhow::Error;
    fn from_str(source: &str) -> Result<Self> {
        ensure!(source.len() <= MAX_TEXT_BYTES, "IR text exceeds size limit");
        let mut p = Parser { source, offset: 0 };
        let result = p.expression();
        let prefix = &source[..p.offset];
        let line = prefix.bytes().filter(|b| *b == b'\n').count() + 1;
        let column = prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1;
        result.map_err(|e| anyhow::anyhow!("IR {line}:{column}: {e:#}"))
    }
}
struct Parser<'a> {
    source: &'a str,
    offset: usize,
}
impl Parser<'_> {
    fn rest(&self) -> &str {
        &self.source[self.offset..]
    }
    fn skip(&mut self) {
        loop {
            while self.rest().starts_with(char::is_whitespace) {
                self.offset += self.rest().chars().next().unwrap().len_utf8();
            }
            if self.rest().starts_with("//") {
                self.offset += self.rest().find('\n').unwrap_or(self.rest().len());
            } else {
                break;
            }
        }
    }
    fn take(&mut self, token: &str) -> bool {
        self.skip();
        if self.rest().starts_with(token) {
            self.offset += token.len();
            true
        } else {
            false
        }
    }
    fn expect(&mut self, token: &str) -> Result<()> {
        ensure!(self.take(token), "expected {token:?}");
        Ok(())
    }
    fn name(&mut self) -> Result<String> {
        self.skip();
        let n = self
            .rest()
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || b"_.-".contains(b))
            .count();
        let name = self.rest()[..n].to_owned();
        symbol(&name)?;
        self.offset += n;
        Ok(name)
    }
    fn json<T: serde::de::DeserializeOwned>(&mut self) -> Result<T> {
        self.skip();
        let mut stream = serde_json::Deserializer::from_str(self.rest()).into_iter::<T>();
        let value = stream
            .next()
            .ok_or_else(|| anyhow::anyhow!("expected JSON value"))??;
        self.offset += stream.byte_offset();
        Ok(value)
    }
    fn value(&mut self) -> Result<Value> {
        self.skip();
        if self.rest().starts_with(|c: char| c.is_ascii_digit()) {
            let length = self.rest().bytes().take_while(u8::is_ascii_digit).count();
            let value = self.rest()[..length].parse()?;
            self.offset += length;
            Ok(Value::U64(value))
        } else {
            self.expect("bytes")?;
            self.expect("(")?;
            let data = self.json()?;
            self.expect(")")?;
            Ok(Value::Bytes(data))
        }
    }
    fn expression(&mut self) -> Result<Expression> {
        let name = self.name()?;
        self.expect("(")?;
        let file: String = self.json()?;
        let mut fields = BTreeMap::new();
        while self.take(",") {
            let key = self.name()?;
            self.expect(":")?;
            ensure!(
                fields.insert(key.clone(), self.value()?).is_none(),
                "duplicate argument {key}"
            );
        }
        self.expect(")")?;
        let Some(Value::U64(offset)) = fields.remove("offset") else {
            bail!("offset must be u64");
        };
        let operation = match name.as_str() {
            "fs.read" => {
                let Some(Value::U64(length)) = fields.remove("length") else {
                    bail!("length must be u64");
                };
                Operation::Read {
                    file,
                    offset,
                    length,
                }
            }
            "fs.write" => {
                let Some(Value::Bytes(data)) = fields.remove("data") else {
                    bail!("data must be bytes");
                };
                Operation::Write { file, offset, data }
            }
            _ => bail!("unknown operation {name}"),
        };
        ensure!(fields.is_empty(), "unknown operation arguments");
        let mut contexts = Vec::new();
        while self.take("|>") {
            ensure!(contexts.len() < MAX_CONTEXTS, "too many contexts");
            let name = self.name()?;
            self.expect("(")?;
            let context = match name.as_str() {
                "vm" => Layer::Vm { name: self.json()? },
                "remote" => Layer::Remote { name: self.json()? },
                "overlay" => Layer::Overlay { name: self.json()? },
                "mock" => Layer::Mock {
                    value: self.value()?,
                },
                "deny" => Layer::Deny {
                    reason: self.json()?,
                },
                _ => bail!("unknown context {name}"),
            };
            self.expect(")")?;
            contexts.push(context);
        }
        self.skip();
        ensure!(self.rest().is_empty(), "trailing input");
        let expr = Expression {
            version: VERSION,
            operation,
            contexts,
        };
        expr.validate()?;
        Ok(expr)
    }
}
