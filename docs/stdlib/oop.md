# Structs, Methods & Objects (OOP)

Ran supports record-style OOP: structs with fields, instance methods, and
associated functions (constructors). Objects use **value semantics** — methods
return updated copies rather than mutating in place. For money and business
state, immutability is a feature: you cannot accidentally alias and corrupt a
balance.

## Defining a struct

```ran
struct Account {
    owner: str,
    balance: decimal,
}
```

## Constructing

Struct literals use `Name { field: value, ... }`:

```ran
let acc = Account { owner: "Risqi", balance: dec("100.00") }
```

Inside `if`/`while`/`for` headers, `{` starts the body, so wrap a struct literal
in parentheses there: `if (Account { ... }).balance > 0 { }`.

## Field access & interpolation

```ran
echo acc.owner                 # Risqi
echo "balance is $acc.balance" # dotted paths work in interpolation
```

## Methods (`impl`)

The first parameter `self` is the receiver. Methods return values (often a new
instance):

```ran
impl Account {
    fn deposit(self, amount) -> Account {
        return Account { owner: self.owner, balance: self.balance + amount }
    }

    fn withdraw(self, amount) -> Account {
        if self.balance.cmp(amount) < 0 {
            return self        # insufficient funds: unchanged
        }
        return Account { owner: self.owner, balance: self.balance - amount }
    }

    fn describe(self) -> str {
        return self.owner + ": " + self.balance
    }
}

let a = Account { owner: "Risqi", balance: dec("100.00") }
let b = a.deposit(dec("50.25"))   # b.balance == 150.25, a unchanged
```

## Associated functions (constructors / static methods)

A method **without** a `self` parameter is called on the type name:

```ran
impl Account {
    fn open(owner) -> Account {
        return Account { owner: owner, balance: dec("0.00") }
    }
}

let fresh = Account.open("Alice")
```

## typeof

`typeof(value)` returns the struct's name for objects:

```ran
echo typeof(a)   # Account
```

## Traits (`trait` + `impl Trait for Type`)

A `trait` declares a set of methods (optionally with default bodies). A type opts in
with `impl Trait for Type`; a method call dispatches to the implementation registered
for the receiver value's type. A type that does not override a defaulted method
inherits the trait's default.

```ran
trait Shape {
    fn sides(self) -> int
    fn describe(self) -> str { return "a shape with sides" }  # default body
}

struct Triangle { x: int }
struct Square { x: int }

impl Shape for Triangle { fn sides(self) -> int { return 3 } }
impl Shape for Square {
    fn sides(self) -> int { return 4 }
    fn describe(self) -> str { return "a square" }   # overrides the default
}

fn main() {
    let t = Triangle { x: 0 }
    let s = Square { x: 0 }
    echo t.sides()       # 3
    echo s.sides()       # 4
    echo t.describe()    # a shape with sides   (default)
    echo s.describe()    # a square             (override)
}
```

## Current limitations (roadmap)

- No field mutation syntax (`a.balance = x`); use functional updates / returns or a
  `&mut` parameter (which writes back to the caller).
- No field type-checking at construction time.
- Trait objects / dynamic dispatch behind a single static type are not modeled; dispatch
  is by the concrete receiver type.

`enum` + `match` execute (including `return` from a `match` arm), and traits +
`impl Trait for Type` are supported. For business modeling, structs + methods +
traits + `decimal` cover `Account`, `Order`, `Invoice`, `LineItem`, etc. cleanly.
