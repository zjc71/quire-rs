//! Small, dependency-free formula evaluator for the sheet view.
//!
//! Supported: arithmetic, parentheses, cell references, ranges and the
//! aggregate functions SUM, AVERAGE, MIN, MAX and COUNT. This is deliberately a
//! bounded value evaluator, not a full spreadsheet calculation engine.

use std::collections::BTreeSet;

use crate::sheet::SheetGrid;

enum Token {
    Number(f64),
    Ident(String),
    Plus,
    Minus,
    Star,
    Slash,
    Caret,
    LeftParen,
    RightParen,
    Comma,
    Colon,
}

struct Lexer {
    tokens: Vec<Token>,
    position: usize,
}

impl Lexer {
    fn new(input: &str) -> Result<Self, String> {
        let chars: Vec<char> = input.chars().collect();
        let mut tokens = Vec::new();
        let mut index = 0usize;
        while index < chars.len() {
            let ch = chars[index];
            if ch.is_whitespace() {
                index += 1;
                continue;
            }
            match ch {
                '+' => {
                    tokens.push(Token::Plus);
                    index += 1;
                }
                '-' => {
                    tokens.push(Token::Minus);
                    index += 1;
                }
                '*' => {
                    tokens.push(Token::Star);
                    index += 1;
                }
                '/' => {
                    tokens.push(Token::Slash);
                    index += 1;
                }
                '^' => {
                    tokens.push(Token::Caret);
                    index += 1;
                }
                '(' => {
                    tokens.push(Token::LeftParen);
                    index += 1;
                }
                ')' => {
                    tokens.push(Token::RightParen);
                    index += 1;
                }
                ',' => {
                    tokens.push(Token::Comma);
                    index += 1;
                }
                ':' => {
                    tokens.push(Token::Colon);
                    index += 1;
                }
                ch if ch.is_ascii_digit() || ch == '.' => {
                    let start = index;
                    index += 1;
                    while index < chars.len()
                        && (chars[index].is_ascii_digit() || chars[index] == '.')
                    {
                        index += 1;
                    }
                    let text: String = chars[start..index].iter().collect();
                    let value = text
                        .parse::<f64>()
                        .map_err(|_| "#VALUE!".to_string())?;
                    tokens.push(Token::Number(value));
                }
                ch if ch.is_ascii_alphabetic() || ch == '_' || ch == '$' => {
                    let start = index;
                    index += 1;
                    while index < chars.len()
                        && (chars[index].is_ascii_alphanumeric()
                            || chars[index] == '_'
                            || chars[index] == '$'
                            || chars[index] == '.')
                    {
                        index += 1;
                    }
                    tokens.push(Token::Ident(chars[start..index].iter().collect()));
                }
                _ => return Err("#VALUE!".to_string()),
            }
        }
        Ok(Self {
            tokens,
            position: 0,
        })
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.position)
    }

    fn next(&mut self) -> Option<&Token> {
        let token = self.tokens.get(self.position);
        if token.is_some() {
            self.position += 1;
        }
        token
    }
}

enum Expr {
    Number(f64),
    Reference(usize, usize),
    Range((usize, usize), (usize, usize)),
    Unary(char, Box<Expr>),
    Binary(char, Box<Expr>, Box<Expr>),
    Function(String, Vec<Expr>),
}

enum Value {
    Number(f64),
    Range((usize, usize), (usize, usize)),
}

fn parse_column(text: &str) -> Option<usize> {
    let mut value = 0usize;
    let mut seen = false;
    for ch in text.chars() {
        if !ch.is_ascii_alphabetic() {
            break;
        }
        value = value * 26 + (ch.to_ascii_uppercase() as usize - 'A' as usize + 1);
        seen = true;
    }
    seen.then(|| value.saturating_sub(1))
}

fn parse_reference(text: &str) -> Option<(usize, usize)> {
    let cleaned: String = text.chars().filter(|ch| *ch != '$').collect();
    if cleaned.is_empty() {
        return None;
    }
    let split = cleaned
        .char_indices()
        .find(|(_, ch)| ch.is_ascii_digit())
        .map(|(index, _)| index)?;
    let (letters, digits) = cleaned.split_at(split);
    if digits.is_empty() || !digits.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    let column = parse_column(letters)?;
    let row = digits.parse::<usize>().ok()?;
    (row > 0).then(|| (row - 1, column))
}

struct Parser {
    lexer: Lexer,
}

impl Parser {
    fn new(input: &str) -> Result<Self, String> {
        Ok(Self {
            lexer: Lexer::new(input)?,
        })
    }

    fn parse(mut self) -> Result<Expr, String> {
        let expression = self.expression()?;
        if self.lexer.peek().is_some() {
            return Err("#VALUE!".to_string());
        }
        Ok(expression)
    }

    fn expression(&mut self) -> Result<Expr, String> {
        self.additive()
    }

    fn additive(&mut self) -> Result<Expr, String> {
        let mut left = self.multiplicative()?;
        loop {
            let operator = match self.lexer.peek() {
                Some(Token::Plus) => '+',
                Some(Token::Minus) => '-',
                _ => break,
            };
            self.lexer.next();
            let right = self.multiplicative()?;
            left = Expr::Binary(operator, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn multiplicative(&mut self) -> Result<Expr, String> {
        let mut left = self.unary()?;
        loop {
            let operator = match self.lexer.peek() {
                Some(Token::Star) => '*',
                Some(Token::Slash) => '/',
                _ => break,
            };
            self.lexer.next();
            let right = self.unary()?;
            left = Expr::Binary(operator, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn power(&mut self) -> Result<Expr, String> {
        let left = self.primary()?;
        if matches!(self.lexer.peek(), Some(Token::Caret)) {
            self.lexer.next();
            let right = self.unary()?;
            return Ok(Expr::Binary('^', Box::new(left), Box::new(right)));
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Expr, String> {
        match self.lexer.peek() {
            Some(Token::Plus) => {
                self.lexer.next();
                Ok(Expr::Unary('+', Box::new(self.unary()?)))
            }
            Some(Token::Minus) => {
                self.lexer.next();
                Ok(Expr::Unary('-', Box::new(self.unary()?)))
            }
            _ => self.power(),
        }
    }

    fn primary(&mut self) -> Result<Expr, String> {
        match self.lexer.next() {
            Some(Token::Number(value)) => Ok(Expr::Number(*value)),
            Some(Token::LeftParen) => {
                let expression = self.expression()?;
                if !matches!(self.lexer.next(), Some(Token::RightParen)) {
                    return Err("#VALUE!".to_string());
                }
                Ok(expression)
            }
            Some(Token::Ident(name)) => {
                let name = name.clone();
                if matches!(self.lexer.peek(), Some(Token::LeftParen)) {
                    self.lexer.next();
                    let mut arguments = Vec::new();
                    if !matches!(self.lexer.peek(), Some(Token::RightParen)) {
                        loop {
                            arguments.push(self.expression()?);
                            match self.lexer.peek() {
                                Some(Token::Comma) => {
                                    self.lexer.next();
                                }
                                Some(Token::RightParen) => break,
                                _ => return Err("#VALUE!".to_string()),
                            }
                        }
                    }
                    if !matches!(self.lexer.next(), Some(Token::RightParen)) {
                        return Err("#VALUE!".to_string());
                    }
                    return Ok(Expr::Function(name.to_ascii_uppercase(), arguments));
                }
                let start = parse_reference(&name)
                    .ok_or_else(|| "#NAME?".to_string())?;
                if matches!(self.lexer.peek(), Some(Token::Colon)) {
                    self.lexer.next();
                    let Some(Token::Ident(end)) = self.lexer.next() else {
                        return Err("#VALUE!".to_string());
                    };
                    let end = parse_reference(end)
                        .ok_or_else(|| "#REF!".to_string())?;
                    return Ok(Expr::Range(start, end));
                }
                Ok(Expr::Reference(start.0, start.1))
            }
            _ => Err("#VALUE!".to_string()),
        }
    }
}

fn cell_number(
    grid: &SheetGrid,
    row: usize,
    column: usize,
    stack: &mut BTreeSet<(usize, usize)>,
) -> Result<f64, String> {
    let raw = grid.cell(row, column);
    if raw.trim_start().starts_with('=') {
        if !stack.insert((row, column)) {
            return Err("#CYCLE!".to_string());
        }
        let result = Parser::new(raw.trim_start().trim_start_matches('='))
            .and_then(|parser| parser.parse())
            .and_then(|expression| evaluate_expression(grid, &expression, stack));
        stack.remove(&(row, column));
        match result? {
            Value::Number(value) => Ok(value),
            Value::Range(_, _) => Err("#VALUE!".to_string()),
        }
    } else {
        let text = raw.trim();
        if text.is_empty() {
            Ok(0.0)
        } else {
            text.parse::<f64>()
                .map_err(|_| "#VALUE!".to_string())
        }
    }
}

fn evaluate_expression(
    grid: &SheetGrid,
    expression: &Expr,
    stack: &mut BTreeSet<(usize, usize)>,
) -> Result<Value, String> {
    match expression {
        Expr::Number(value) => Ok(Value::Number(*value)),
        Expr::Reference(row, column) => {
            Ok(Value::Number(cell_number(grid, *row, *column, stack)?))
        }
        Expr::Range(start, end) => Ok(Value::Range(*start, *end)),
        Expr::Unary(operator, inner) => {
            let value = match evaluate_expression(grid, inner, stack)? {
                Value::Number(value) => value,
                Value::Range(_, _) => return Err("#VALUE!".to_string()),
            };
            Ok(Value::Number(if *operator == '-' { -value } else { value }))
        }
        Expr::Binary(operator, left, right) => {
            let left = match evaluate_expression(grid, left, stack)? {
                Value::Number(value) => value,
                Value::Range(_, _) => return Err("#VALUE!".to_string()),
            };
            let right = match evaluate_expression(grid, right, stack)? {
                Value::Number(value) => value,
                Value::Range(_, _) => return Err("#VALUE!".to_string()),
            };
            let value = match operator {
                '+' => left + right,
                '-' => left - right,
                '*' => left * right,
                '/' => {
                    if right == 0.0 {
                        return Err("#DIV/0!".to_string());
                    }
                    left / right
                }
                '^' => left.powf(right),
                _ => return Err("#VALUE!".to_string()),
            };
            Ok(Value::Number(value))
        }
        Expr::Function(name, arguments) => {
            let mut numbers = Vec::new();
            for argument in arguments {
                match evaluate_expression(grid, argument, stack)? {
                    Value::Number(value) => numbers.push(value),
                    Value::Range(start, end) => {
                        let top = start.0.min(end.0);
                        let bottom = start.0.max(end.0);
                        let left = start.1.min(end.1);
                        let right = start.1.max(end.1);
                        for row in top..=bottom {
                            for column in left..=right {
                                let raw = grid.cell(row, column);
                                if raw.trim().is_empty() {
                                    continue;
                                }
                                if raw.trim_start().starts_with('=') {
                                    numbers.push(cell_number(grid, row, column, stack)?);
                                } else if let Ok(value) = raw.trim().parse::<f64>() {
                                    numbers.push(value);
                                }
                            }
                        }
                    }
                }
            }
            let value = match name.as_str() {
                "SUM" => numbers.iter().sum(),
                "AVERAGE" => {
                    if numbers.is_empty() {
                        return Err("#DIV/0!".to_string());
                    }
                    numbers.iter().sum::<f64>() / numbers.len() as f64
                }
                "MIN" => numbers.iter().copied().fold(f64::INFINITY, f64::min),
                "MAX" => numbers
                    .iter()
                    .copied()
                    .fold(f64::NEG_INFINITY, f64::max),
                "COUNT" => numbers.len() as f64,
                _ => return Err("#NAME?".to_string()),
            };
            if value.is_infinite() {
                return Ok(Value::Number(0.0));
            }
            Ok(Value::Number(value))
        }
    }
}

fn format_value(value: f64) -> String {
    if !value.is_finite() {
        return "#NUM!".to_string();
    }
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        let mut text = format!("{value:.10}");
        while text.ends_with('0') {
            text.pop();
        }
        if text.ends_with('.') {
            text.pop();
        }
        text
    }
}

pub(crate) fn display_cell(grid: &SheetGrid, row: usize, column: usize) -> String {
    let raw = grid.cell(row, column);
    let trimmed = raw.trim_start();
    if !trimmed.starts_with('=') {
        return raw.to_string();
    }
    match Parser::new(trimmed.trim_start_matches('='))
        .and_then(|parser| parser.parse())
        .and_then(|expression| {
            let mut stack = BTreeSet::new();
            evaluate_expression(grid, &expression, &mut stack)
        }) {
        Ok(Value::Number(value)) => format_value(value),
        Ok(Value::Range(_, _)) => "#VALUE!".to_string(),
        Err(error) => error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(values: &[&str]) -> SheetGrid {
        SheetGrid {
            name: "Sheet1".to_string(),
            rows: vec![values.iter().map(|value| value.to_string()).collect()],
        }
    }

    #[test]
    fn evaluates_arithmetic_and_functions() {
        let sheet = grid(&["1", "2", "=A1+B1", "=SUM(A1:B1)", "=AVERAGE(A1:B1)"]);
        assert_eq!(display_cell(&sheet, 0, 2), "3");
        assert_eq!(display_cell(&sheet, 0, 3), "3");
        assert_eq!(display_cell(&sheet, 0, 4), "1.5");
        assert_eq!(display_cell(&sheet, 0, 5), "");
    }

    #[test]
    fn reports_cycles_and_division_by_zero() {
        let sheet = grid(&["=A1", "=1/0", "=NOPE(1)"]);
        assert_eq!(display_cell(&sheet, 0, 0), "#CYCLE!");
        assert_eq!(display_cell(&sheet, 0, 1), "#DIV/0!");
        assert_eq!(display_cell(&sheet, 0, 2), "#NAME?");
    }

    #[test]
    fn evaluates_references_across_a_range() {
        let sheet = SheetGrid {
            name: "Sheet1".to_string(),
            rows: vec![
                vec!["1".to_string(), "2".to_string()],
                vec!["3".to_string(), "4".to_string()],
                vec!["=SUM(A1:B2)".to_string()],
            ],
        };
        assert_eq!(display_cell(&sheet, 2, 0), "10");
    }

    #[test]
    fn respects_unary_and_power_precedence() {
        let sheet = grid(&["=-2^2", "=2^-2"]);
        assert_eq!(display_cell(&sheet, 0, 0), "-4");
        assert_eq!(display_cell(&sheet, 0, 1), "0.25");
    }
}
