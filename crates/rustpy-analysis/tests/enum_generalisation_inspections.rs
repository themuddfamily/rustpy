use rustpy_analysis::inspections::{self, InspectionOptions};
use rustpy_semantic::db::Analysis;
use std::path::Path;

fn problems(source: &str) -> Vec<String> {
    let db = Analysis::for_project(Path::new("/enum-generalisation-inspections"));
    let id = db.set_file_text(Path::new("/enum-generalisation-inspections/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    inspections::inspect(&db, &file, &InspectionOptions::default())
        .into_iter()
        .filter(|problem| {
            matches!(
                problem.diagnostic.code.as_str(),
                "PyUnresolvedReferences"
                    | "PyTypeChecker"
                    | "PyArgumentList"
                    | "PyCallingNonCallable"
                    | "PyTypedDict"
                    | "PyNoneFunctionAssignment"
            )
        })
        .map(|problem| format!("{problem:?}"))
        .collect()
}

#[test]
fn generalised_enums_raise_no_false_attribute_or_type_warnings() {
    let source = r#"from enum import Enum, IntEnum, StrEnum, Flag, EnumMeta, auto, member, nonmember
from typing import TypedDict


class Mode(str, Enum):
    READ = "r"
    WRITE = "w"


class Level(int, Enum):
    LOW = 1
    HIGH = 2


class Base(Enum):
    def describe(self) -> str:
        return self.name.lower()


class Shade(Base):
    DARK = auto()
    LIGHT = auto()


class Meta(metaclass=EnumMeta):
    ONE = 1


class Planet(Enum):
    MERCURY = (3.303e23, 2.4397e6)
    EARTH = (5.976e24, 6.37814e6)

    def __init__(self, mass: float, radius: float) -> None:
        self.mass = mass
        self.radius = radius

    @property
    def gravity(self) -> float:
        return self.mass / (self.radius * self.radius)


class Shape(Enum):
    _ignore_ = ["helper"]
    SQUARE = object()
    ROUND = 1.5
    COUNT = nonmember(2)
    LABEL = member("label")
    helper = 3

    class Inner:
        pass

    def area(self) -> float:
        return 1.0


class Name(StrEnum):
    ALPHA = auto()


class Perm(Flag):
    R = auto()
    W = auto()


Color = Enum("Color", "RED GREEN")
Size = IntEnum("Size", ["S", "M"])
Code = Enum("Code", {"OK": 200})
Movie = TypedDict("Movie", {"title": str, "year": int}, total=False)


def use(movie: Movie) -> None:
    print(Mode.READ.upper(), Mode.READ.value, Mode("r"), Mode["READ"])
    print(Level.LOW + 1, Level.HIGH.value, Level(2).name)
    print(Shade.DARK.describe(), Shade.LIGHT.value, Meta.ONE)
    print(Planet.EARTH.mass, Planet.EARTH.radius, Planet.EARTH.gravity, Planet.MERCURY.value)
    print(Shape.SQUARE.name, Shape.ROUND.value, Shape.COUNT + 1, Shape.LABEL.value)
    print(Shape.Inner, Shape.SQUARE.area(), Name.ALPHA.value.upper(), Perm.R | Perm.W)
    print(Color.RED, Color.GREEN.value, Size.S.value, Code.OK.value, Color["RED"], Size(1))
    for planet in Planet:
        print(planet.mass)
    for color in Color:
        print(color.name)
    print(movie["title"], movie.get("year"))
    copy: Movie = {"title": "x"}
    print(copy, Movie(title="y"))
"#;
    let found = problems(source);
    assert!(found.is_empty(), "{found:#?}");
}
