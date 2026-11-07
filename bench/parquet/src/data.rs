use std::sync::Arc;

use arrow_schema::{DataType, Field, Fields, Schema, SchemaRef};
use serde_json::{Map, Value, json};

pub type Doc = Map<String, Value>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Small,
    Medium,
}

impl Shape {
    pub fn parse(text: &str) -> Result<Self, String> {
        match text {
            "small" => Ok(Self::Small),
            "medium" => Ok(Self::Medium),
            other => Err(format!("unknown shape {other}")),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Medium => "medium",
        }
    }

    pub fn arrow_schema(self) -> SchemaRef {
        let mut fields = vec![
            text_field("name"),
            text_field("email"),
            Field::new("age", DataType::Int64, true),
            text_field("city"),
            Field::new("bucket", DataType::Int64, true),
            Field::new("score", DataType::Float64, true),
            Field::new("active", DataType::Boolean, true),
            Field::new("created", DataType::Int64, true),
            text_field("tag"),
        ];
        if self == Self::Medium {
            fields.extend(Self::medium_fields());
        }
        Arc::new(Schema::new(fields))
    }

    fn medium_fields() -> Vec<Field> {
        let author = Fields::from(vec![
            text_field("name"),
            text_field("email"),
            text_field("org"),
        ]);
        let stats = Fields::from(vec![
            Field::new("views", DataType::Int64, true),
            Field::new("likes", DataType::Int64, true),
            Field::new("rating", DataType::Float64, true),
        ]);
        let address = Fields::from(vec![
            text_field("street"),
            text_field("city"),
            text_field("zip"),
            text_field("country"),
        ]);
        vec![
            text_field("title"),
            text_field("category"),
            Field::new("author", DataType::Struct(author), true),
            Field::new(
                "tags",
                DataType::List(Arc::new(Field::new("item", DataType::Utf8, true))),
                true,
            ),
            Field::new("stats", DataType::Struct(stats), true),
            Field::new("address", DataType::Struct(address), true),
            Field::new(
                "numbers",
                DataType::List(Arc::new(Field::new("item", DataType::Int64, true))),
                true,
            ),
            text_field("body"),
        ]
    }
}

fn text_field(name: &str) -> Field {
    Field::new(name, DataType::Utf8, true)
}

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

const CITIES: [&str; 50] = [
    "Lagos", "Abuja", "Accra", "Nairobi", "Cairo", "Tunis", "Dakar", "Kigali", "Lusaka", "Harare",
    "London", "Paris", "Berlin", "Madrid", "Rome", "Vienna", "Prague", "Warsaw", "Oslo", "Lisbon",
    "Toronto", "Chicago", "Austin", "Denver", "Boston", "Seattle", "Miami", "Atlanta", "Phoenix",
    "Dallas", "Tokyo", "Osaka", "Seoul", "Taipei", "Manila", "Hanoi", "Jakarta", "Delhi", "Mumbai",
    "Dhaka", "Lima", "Quito", "Bogota", "Santiago", "Caracas", "Havana", "Sydney", "Perth",
    "Auckland", "Suva",
];

const WORDS: [&str; 64] = [
    "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india", "juliet",
    "kilo", "lima", "mike", "november", "oscar", "papa", "quebec", "romeo", "sierra", "tango",
    "uniform", "victor", "whiskey", "xray", "yankee", "zulu", "amber", "basil", "cedar", "dune",
    "ember", "fjord", "grove", "harbor", "island", "jungle", "kettle", "lantern", "meadow",
    "nectar", "orchid", "pebble", "quartz", "river", "summit", "timber", "umber", "valley",
    "willow", "yarrow", "zephyr", "atlas", "beacon", "canyon", "delta", "estuary", "falcon",
    "glacier", "horizon", "inlet", "jasper", "krypton", "lagoon", "monsoon",
];

pub struct Generator {
    shape: Shape,
    rng: Rng,
}

impl Generator {
    pub fn new(shape: Shape, seed: u64) -> Self {
        Self {
            shape,
            rng: Rng::new(seed),
        }
    }

    pub fn doc(&mut self, index: u64) -> Doc {
        let mut doc = self.small(index);
        if self.shape == Shape::Medium {
            self.add_medium(index, &mut doc);
        }
        doc
    }

    fn word(&mut self) -> &'static str {
        WORDS[self.rng.below(64) as usize]
    }

    fn small(&mut self, index: u64) -> Doc {
        let value = json!({
            "name": format!("user-{index}"),
            "email": format!("user{index}@example.com"),
            "age": self.rng.below(100),
            "city": CITIES[self.rng.below(50) as usize],
            "bucket": self.rng.below(1000),
            "score": self.rng.below(10_000) as f64 / 100.0,
            "active": self.rng.below(2) == 0,
            "created": 1_700_000_000_000_u64 + index * 1000,
            "tag": self.word(),
        });
        value.as_object().cloned().unwrap_or_default()
    }

    fn sentence(&mut self, words: usize) -> String {
        let mut text = String::new();
        for position in 0..words {
            if position > 0 {
                text.push(' ');
            }
            text.push_str(self.word());
        }
        text
    }

    fn add_medium(&mut self, index: u64, doc: &mut Doc) {
        let tags: Vec<&str> = (0..5).map(|_| self.word()).collect();
        let numbers: Vec<u64> = (0..10).map(|_| self.rng.below(1_000_000)).collect();
        let extra = json!({
            "title": self.sentence(8),
            "category": format!("category-{}", self.rng.below(20)),
            "author": {
                "name": format!("author-{}", self.rng.below(5000)),
                "email": format!("author{}@example.org", self.rng.below(5000)),
                "org": format!("org-{}", self.rng.below(300)),
            },
            "tags": tags,
            "stats": {
                "views": self.rng.below(1_000_000),
                "likes": self.rng.below(50_000),
                "rating": self.rng.below(500) as f64 / 100.0,
            },
            "address": {
                "street": format!("{} {} street", self.rng.below(9999), self.word()),
                "city": CITIES[self.rng.below(50) as usize],
                "zip": format!("{:05}", self.rng.below(100_000)),
                "country": format!("country-{}", self.rng.below(60)),
            },
            "numbers": numbers,
            "body": self.sentence(300 + (index % 50) as usize),
        });
        if let Some(fields) = extra.as_object() {
            for (key, value) in fields {
                doc.insert(key.clone(), value.clone());
            }
        }
    }
}

pub fn project(doc: &Doc, fields: &[&str]) -> Doc {
    let mut projected = Doc::new();
    for field in fields {
        if let Some(value) = doc.get(*field) {
            projected.insert((*field).to_owned(), value.clone());
        }
    }
    projected
}
