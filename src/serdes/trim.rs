use crate::errors::DatabaseError;
use crate::serdes::Serialization;
use sqlparser::ast::TrimWhereField;
use std::io::{Read, Write};

impl Serialization for TrimWhereField {
    type Error = DatabaseError;

    fn encode<W: Write>(&self, writer: &mut W) -> Result<(), Self::Error> {
        let type_id = match self {
            TrimWhereField::Both => 0,
            TrimWhereField::Leading => 1,
            TrimWhereField::Trailing => 2,
        };
        writer.write_all(&[type_id])?;

        Ok(())
    }

    fn decode<R: Read>(reader: &mut R) -> Result<Self, Self::Error> {
        let mut one_byte = [0u8; 1];
        reader.read_exact(&mut one_byte)?;

        Ok(match one_byte[0] {
            0 => TrimWhereField::Both,
            1 => TrimWhereField::Leading,
            2 => TrimWhereField::Trailing,
            _ => unreachable!(),
        })
    }
}