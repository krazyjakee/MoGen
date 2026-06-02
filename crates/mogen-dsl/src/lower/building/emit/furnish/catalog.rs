//! The furnishing catalog: which props belong in which kind of room, where
//! they sit, and how big a footprint they reserve.
//!
//! This is the "comprehensive list of objects per group" half of the feature.
//! A `Category` is a *function* a room performs (bedroom, kitchen, server
//! room, …), not a building type — the same office tower has bedrooms in its
//! hotel floors and a server room in its basement. `classify` maps the
//! author's free-text `room_type "name"` onto a category by keyword, so an
//! LLM or a human can name rooms naturally and still get sensible props.
//!
//! Everything here is data only: the placement engine in the parent module
//! turns an `Item` list into marker transforms. Each marker is a geometry-free
//! POI — the engine target swaps in its own prefab — so the catalog names
//! roles (`"bed"`, `"stove"`) rather than describing meshes.

use super::super::super::config::RoomKind;

/// Where a prop sits in its room. Drives the placement engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Place {
    /// Against a wall, back to the wall, facing into the room. Distributed
    /// around the perimeter by a best-fit packer (`width` is the run it
    /// reserves along the wall).
    Wall,
    /// Tucked into a corner, facing diagonally inward. Round-robined across
    /// the four corners.
    Corner,
    /// Free-standing near the room centre (dining tables, rugs, pool tables).
    Centre,
    /// Dropped at a random interior point (clutter, boxes, loose chairs).
    Scatter,
    /// Mounted on the ceiling at `y = ceiling_height`, laid out in a grid.
    Ceiling,
}

/// One catalog entry: a prop kind plus how to place and how many.
#[derive(Clone, Copy, Debug)]
pub(super) struct Item {
    /// POI `role` string the marker carries (`"bed"`, `"desk"`, …).
    pub role: &'static str,
    pub place: Place,
    /// Footprint reserved along a wall, metres. Only meaningful for `Wall`;
    /// other placements use it as a rough clearance radius.
    pub width: f32,
    /// Skip this item when the room floor area (m²) is below this. Keeps a
    /// broom cupboard from sprouting a sofa.
    pub min_area: f32,
    /// Base count.
    pub n: u32,
    /// Extra copies per m² of floor area, added to `n` and capped at `cap`.
    /// `0.0` means "always exactly `n`".
    pub per_m2: f32,
    /// Hard cap on the final count.
    pub cap: u32,
    /// Height of the marker above the floor, metres. `0.0` = on the floor;
    /// used for wall-mounted props (TV, whiteboard, clock) and ceiling fixtures.
    pub y: f32,
}

impl Item {
    const fn base(role: &'static str, place: Place) -> Self {
        Item { role, place, width: 0.8, min_area: 0.0, n: 1, per_m2: 0.0, cap: 1, y: 0.0 }
    }
    const fn wall(role: &'static str) -> Self {
        Self::base(role, Place::Wall)
    }
    const fn corner(role: &'static str) -> Self {
        Self::base(role, Place::Corner).w(0.5)
    }
    const fn centre(role: &'static str) -> Self {
        Self::base(role, Place::Centre).w(1.2)
    }
    const fn scatter(role: &'static str) -> Self {
        Self::base(role, Place::Scatter).w(0.6)
    }
    const fn ceiling(role: &'static str) -> Self {
        Self::base(role, Place::Ceiling).w(1.0)
    }
    const fn w(mut self, v: f32) -> Self {
        self.width = v;
        self
    }
    const fn area(mut self, v: f32) -> Self {
        self.min_area = v;
        self
    }
    /// Scale count with area: start at `n`, add `per` per m², cap at `cap`.
    const fn many(mut self, n: u32, per: f32, cap: u32) -> Self {
        self.n = n;
        self.per_m2 = per;
        self.cap = cap;
        self
    }
    const fn at_y(mut self, v: f32) -> Self {
        self.y = v;
        self
    }

    /// Final count for a room of `area` m².
    pub(super) fn count(&self, area: f32) -> u32 {
        let extra = (area * self.per_m2).floor() as u32;
        self.n.saturating_add(extra).min(self.cap.max(self.n))
    }
}

/// A room's function — the axis the catalog is keyed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Category {
    Bedroom,
    Bathroom,
    Kitchen,
    Pantry,
    Dining,
    Living,
    Office,
    Meeting,
    Reception,
    Lobby,
    Corridor,
    Storage,
    Closet,
    Garage,
    Workshop,
    Laundry,
    Utility,
    ServerRoom,
    Retail,
    Warehouse,
    Classroom,
    Library,
    Lab,
    Medical,
    Ward,
    Gym,
    Restaurant,
    Bar,
    Cell,
    Generic,
}

impl Category {
    /// Lower-case tag stem used on the per-room furniture group
    /// (`furniture` group tag `cat=<this>`).
    pub(super) fn tag(self) -> &'static str {
        use Category::*;
        match self {
            Bedroom => "bedroom",
            Bathroom => "bathroom",
            Kitchen => "kitchen",
            Pantry => "pantry",
            Dining => "dining",
            Living => "living",
            Office => "office",
            Meeting => "meeting",
            Reception => "reception",
            Lobby => "lobby",
            Corridor => "corridor",
            Storage => "storage",
            Closet => "closet",
            Garage => "garage",
            Workshop => "workshop",
            Laundry => "laundry",
            Utility => "utility",
            ServerRoom => "server_room",
            Retail => "retail",
            Warehouse => "warehouse",
            Classroom => "classroom",
            Library => "library",
            Lab => "lab",
            Medical => "medical",
            Ward => "ward",
            Gym => "gym",
            Restaurant => "restaurant",
            Bar => "bar",
            Cell => "cell",
            Generic => "generic",
        }
    }
}

/// Map an author's `room_type` name (and its `kind`) onto a [`Category`].
///
/// Keyword match on a lower-cased copy of the name — substrings so
/// `"master bedroom"`, `"guest_bed"` and `"BedRoom"` all land on `Bedroom`.
/// Order matters: more specific stems come first (a "staff kitchenette"
/// should read as a kitchen, not a generic staff room). Falls back to the
/// declared `RoomKind` when no keyword matches.
pub(super) fn classify(name: &str, kind: RoomKind) -> Category {
    use Category::*;
    let n = name.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|k| n.contains(k));

    // Specific functional rooms first.
    if has(&["bathroom", "toilet", "washroom", "restroom", "lavatory", "ensuite", "en-suite", "shower", " wc", "wc ", "powder"]) {
        return Bathroom;
    }
    if has(&["server", "data centre", "data center", "datacenter", "comms", "network room", "it room", "rack room"]) {
        return ServerRoom;
    }
    if has(&["pantry", "larder"]) {
        return Pantry;
    }
    if has(&["kitchen", "galley", "kitchenette"]) {
        return Kitchen;
    }
    if has(&["bedroom", "bed room", "dorm", "guest room", "guestroom", "master", "berth", "cabin"]) || n == "bed" {
        return Bedroom;
    }
    if has(&["nursery"]) {
        return Bedroom;
    }
    if has(&["meeting", "conference", "boardroom", "board room"]) {
        return Meeting;
    }
    if has(&["classroom", "class room", "lecture", "seminar", "tutorial"]) || n == "class" {
        return Classroom;
    }
    if has(&["laborator", "lab ", " lab", "research", "cleanroom", "clean room"]) || n == "lab" {
        return Lab;
    }
    if has(&["library", "archive", "reading"]) {
        return Library;
    }
    if has(&["ward", "patient", "recovery", "icu", "intensive care"]) {
        return Ward;
    }
    if has(&["clinic", "exam", "treatment", "surgery", "operating", "infirmary", "medical", "doctor", "dental", "consult"]) {
        return Medical;
    }
    if has(&["server", "telecom"]) {
        return ServerRoom;
    }
    if has(&["restaurant", "diner", "canteen", "cafeteria", "mess hall"]) {
        return Restaurant;
    }
    if has(&["cafe", "café", "coffee", "tea room", "tearoom"]) {
        return Restaurant;
    }
    if has(&["bar", "pub", "tavern", "saloon", "lounge bar"]) {
        return Bar;
    }
    if has(&["gym", "fitness", "exercise", "workout", "weight room"]) {
        return Gym;
    }
    if has(&["warehouse", "depot", "loading", "stockroom", "stock room", "freight"]) {
        return Warehouse;
    }
    if has(&["retail", "shopfloor", "shop floor", "sales floor", "showroom", "boutique", "storefront", "shop", "store front"]) {
        return Retail;
    }
    if has(&["garage", "carport", "parking", "bay"]) {
        return Garage;
    }
    if has(&["workshop", "work shop", "maker", "fabrication", "machine shop"]) {
        return Workshop;
    }
    if has(&["laundry", "washing"]) {
        return Laundry;
    }
    if has(&["mechanical", "boiler", "plant room", "hvac", "switch", "electrical", "riser", "meter"]) {
        return Utility;
    }
    if has(&["dining", "dinning"]) {
        return Dining;
    }
    if has(&["living", "lounge", "sitting", "family room", "den", "parlor", "parlour", "common room", "commons", "rec room", "recreation", "tv room", "snug"]) {
        return Living;
    }
    if has(&["office", "study", "workspace", "work space", "cubicle", "bureau"]) {
        return Office;
    }
    if has(&["reception", "front desk", "waiting", "concierge"]) {
        return Reception;
    }
    if has(&["lobby", "foyer", "atrium", "entrance", "entry", "vestibule", "hall way", "porch"]) {
        return Lobby;
    }
    if has(&["corridor", "hallway", "passage", "landing", "stair", "circulation", "walkway"]) || n == "hall" {
        return Corridor;
    }
    if has(&["closet", "cloak", "wardrobe", "cupboard"]) {
        return Closet;
    }
    if has(&["storage", "store", "stock", "supply", "supplies", "utility closet"]) {
        return Storage;
    }
    if has(&["cell", "holding", "prison", "detention"]) {
        return Cell;
    }
    if has(&["utility", "service"]) {
        return Utility;
    }

    // No keyword hit: fall back to the declared semantic kind.
    match kind {
        RoomKind::Utility => Utility,
        RoomKind::Service => Corridor,
        RoomKind::Secure => Storage,
        RoomKind::StaffOnly => Office,
        RoomKind::Public => Lobby,
        RoomKind::Private => Generic,
    }
}

/// The prop list for a category. Ordered most-important-first: when a small
/// room can't fit everything, the packer keeps the leading entries.
pub(super) fn items(cat: Category) -> &'static [Item] {
    use Category::*;
    match cat {
        Bedroom => BEDROOM,
        Bathroom => BATHROOM,
        Kitchen => KITCHEN,
        Pantry => PANTRY,
        Dining => DINING,
        Living => LIVING,
        Office => OFFICE,
        Meeting => MEETING,
        Reception => RECEPTION,
        Lobby => LOBBY,
        Corridor => CORRIDOR,
        Storage => STORAGE,
        Closet => CLOSET,
        Garage => GARAGE,
        Workshop => WORKSHOP,
        Laundry => LAUNDRY,
        Utility => UTILITY,
        ServerRoom => SERVER_ROOM,
        Retail => RETAIL,
        Warehouse => WAREHOUSE,
        Classroom => CLASSROOM,
        Library => LIBRARY,
        Lab => LAB,
        Medical => MEDICAL,
        Ward => WARD,
        Gym => GYM,
        Restaurant => RESTAURANT,
        Bar => BAR,
        Cell => CELL,
        Generic => GENERIC,
    }
}

// ---------------------------------------------------------------------------
// Per-category prop lists. Widths are rough real-world footprints in metres.
// ---------------------------------------------------------------------------

const BEDROOM: &[Item] = &[
    Item::wall("bed").w(1.7).area(5.0),
    Item::wall("wardrobe").w(1.2).area(7.0),
    Item::wall("dresser").w(1.0).area(6.0),
    Item::wall("nightstand").w(0.5).many(1, 0.0, 2),
    Item::wall("desk").w(1.2).area(11.0),
    Item::wall("bookshelf").w(0.9).area(10.0),
    Item::wall("mirror").w(0.6).at_y(1.4),
    Item::corner("floor_lamp"),
    Item::corner("houseplant").area(8.0),
    Item::wall("laundry_basket").w(0.5).area(9.0),
    Item::centre("rug").area(8.0),
    Item::ceiling("ceiling_light"),
];

const BATHROOM: &[Item] = &[
    Item::wall("toilet").w(0.7),
    Item::wall("sink").w(0.7),
    Item::wall("bathtub").w(1.7).area(6.0),
    Item::wall("shower").w(0.9).area(4.0),
    Item::wall("vanity_cabinet").w(1.0).area(5.0),
    Item::wall("towel_rail").w(0.6).at_y(1.1),
    Item::wall("mirror").w(0.6).at_y(1.5),
    Item::corner("bin"),
    Item::wall("toilet_paper_holder").w(0.2).at_y(0.7).area(0.0),
    Item::ceiling("extractor_fan"),
];

const KITCHEN: &[Item] = &[
    Item::wall("counter").w(1.6).many(1, 0.12, 4),
    Item::wall("sink").w(0.8),
    Item::wall("stove").w(0.8),
    Item::wall("oven").w(0.7).area(7.0),
    Item::wall("fridge").w(0.8),
    Item::wall("cabinet").w(1.0).many(1, 0.08, 4).at_y(1.6),
    Item::wall("dishwasher").w(0.7).area(8.0),
    Item::wall("microwave").w(0.5).at_y(1.4).area(6.0),
    Item::centre("kitchen_island").w(1.6).area(16.0),
    Item::corner("bin"),
    Item::ceiling("ceiling_light").many(1, 0.0, 2),
];

const PANTRY: &[Item] = &[
    Item::wall("shelving_unit").w(1.0).many(2, 0.3, 6),
    Item::wall("dry_goods_rack").w(0.9).area(3.0),
    Item::corner("step_stool"),
    Item::ceiling("ceiling_light"),
];

const DINING: &[Item] = &[
    Item::centre("dining_table").w(1.8),
    Item::scatter("dining_chair").many(4, 0.4, 12).w(0.5),
    Item::wall("sideboard").w(1.4).area(10.0),
    Item::wall("china_cabinet").w(1.1).area(12.0),
    Item::corner("houseplant"),
    Item::wall("artwork").w(0.8).at_y(1.6).area(8.0),
    Item::centre("rug").w(2.4).area(12.0),
    Item::ceiling("chandelier"),
];

const LIVING: &[Item] = &[
    Item::wall("sofa").w(2.0).area(7.0),
    Item::wall("armchair").w(0.9).many(1, 0.05, 3),
    Item::wall("tv_unit").w(1.6),
    Item::wall("tv").w(1.2).at_y(1.1),
    Item::centre("coffee_table").w(1.1).area(9.0),
    Item::wall("bookshelf").w(0.9).many(1, 0.04, 3).area(9.0),
    Item::corner("floor_lamp"),
    Item::corner("houseplant"),
    Item::wall("fireplace").w(1.2).area(16.0),
    Item::centre("rug").w(2.4).area(10.0),
    Item::ceiling("ceiling_light"),
];

const OFFICE: &[Item] = &[
    Item::wall("desk").w(1.5).many(1, 0.08, 8),
    Item::scatter("office_chair").many(1, 0.08, 8).w(0.6),
    Item::wall("filing_cabinet").w(0.6).many(1, 0.03, 4),
    Item::wall("bookshelf").w(0.9).area(8.0),
    Item::wall("whiteboard").w(1.5).at_y(1.3).area(9.0),
    Item::corner("houseplant"),
    Item::corner("water_cooler").area(12.0),
    Item::wall("printer").w(0.7).area(14.0),
    Item::ceiling("ceiling_light").many(1, 0.04, 6),
];

const MEETING: &[Item] = &[
    Item::centre("conference_table").w(2.4),
    Item::scatter("office_chair").many(4, 0.5, 16).w(0.6),
    Item::wall("whiteboard").w(1.8).at_y(1.3),
    Item::wall("projector_screen").w(1.8).at_y(1.6).area(12.0),
    Item::wall("credenza").w(1.4).area(14.0),
    Item::corner("houseplant"),
    Item::ceiling("projector").area(12.0),
    Item::ceiling("ceiling_light").many(1, 0.04, 6),
];

const RECEPTION: &[Item] = &[
    Item::wall("reception_desk").w(2.0),
    Item::scatter("office_chair").w(0.6),
    Item::wall("waiting_sofa").w(1.8).area(8.0),
    Item::wall("armchair").w(0.9).many(1, 0.06, 4),
    Item::centre("coffee_table").w(0.9).area(10.0),
    Item::corner("houseplant").many(1, 0.0, 2),
    Item::wall("magazine_rack").w(0.5).area(9.0),
    Item::wall("signage").w(1.0).at_y(1.8),
    Item::ceiling("ceiling_light").many(1, 0.04, 6),
];

const LOBBY: &[Item] = &[
    Item::wall("bench").w(1.6).many(1, 0.03, 3),
    Item::corner("houseplant").many(2, 0.04, 6),
    Item::wall("info_board").w(1.2).at_y(1.6),
    Item::wall("directory_sign").w(0.8).at_y(1.7).area(10.0),
    Item::scatter("planter").area(20.0).w(0.8),
    Item::centre("rug").w(2.0).area(18.0),
    Item::wall("coat_rack").w(0.6).area(8.0),
    Item::ceiling("pendant_light").many(1, 0.03, 5),
];

const CORRIDOR: &[Item] = &[
    Item::wall("bench").w(1.4).area(6.0),
    Item::corner("houseplant").area(5.0),
    Item::wall("wall_art").w(0.7).at_y(1.6).many(1, 0.05, 4),
    Item::wall("fire_extinguisher").w(0.3).at_y(1.0),
    Item::wall("noticeboard").w(1.0).at_y(1.5).area(8.0),
    Item::ceiling("ceiling_light").many(1, 0.06, 8),
];

const STORAGE: &[Item] = &[
    Item::wall("shelving_unit").w(1.0).many(2, 0.2, 8),
    Item::scatter("storage_box").many(2, 0.4, 16).w(0.5),
    Item::corner("step_ladder"),
    Item::wall("pallet").w(1.2).area(12.0),
    Item::ceiling("ceiling_light").many(1, 0.04, 4),
];

const CLOSET: &[Item] = &[
    Item::wall("clothes_rail").w(1.2).many(1, 0.2, 3),
    Item::wall("shelf").w(1.0).at_y(1.7).many(1, 0.2, 4),
    Item::corner("shoe_rack"),
    Item::ceiling("ceiling_light"),
];

const GARAGE: &[Item] = &[
    Item::centre("car").w(2.0).area(14.0),
    Item::wall("workbench").w(1.8).area(8.0),
    Item::wall("tool_cabinet").w(1.0).area(8.0),
    Item::wall("shelving_unit").w(1.0).many(1, 0.06, 4),
    Item::wall("pegboard").w(1.2).at_y(1.4).area(10.0),
    Item::corner("bicycle"),
    Item::scatter("storage_box").many(1, 0.1, 6).w(0.5),
    Item::ceiling("strip_light").many(1, 0.04, 4),
];

const WORKSHOP: &[Item] = &[
    Item::wall("workbench").w(1.8).many(1, 0.05, 4),
    Item::wall("tool_cabinet").w(1.0).many(1, 0.04, 3),
    Item::wall("pegboard").w(1.4).at_y(1.4),
    Item::centre("machine_tool").w(1.4).area(16.0),
    Item::wall("vice_bench").w(0.9).area(10.0),
    Item::scatter("material_rack").area(12.0).w(0.8),
    Item::corner("shop_vacuum"),
    Item::ceiling("strip_light").many(1, 0.04, 6),
];

const LAUNDRY: &[Item] = &[
    Item::wall("washing_machine").w(0.7).many(1, 0.05, 4),
    Item::wall("tumble_dryer").w(0.7).many(1, 0.05, 4),
    Item::wall("laundry_sink").w(0.6),
    Item::wall("ironing_board").w(1.3).area(6.0),
    Item::wall("drying_rack").w(0.9).area(5.0),
    Item::wall("shelf").w(1.0).at_y(1.6),
    Item::corner("laundry_basket"),
    Item::ceiling("ceiling_light"),
];

const UTILITY: &[Item] = &[
    Item::wall("boiler").w(0.8),
    Item::wall("electrical_panel").w(0.8).at_y(1.4),
    Item::wall("water_heater").w(0.7).area(4.0),
    Item::wall("hvac_unit").w(1.2).area(8.0),
    Item::wall("fuse_box").w(0.4).at_y(1.5),
    Item::wall("pipe_manifold").w(0.6).at_y(1.2).area(6.0),
    Item::corner("mop_bucket"),
    Item::ceiling("strip_light"),
];

const SERVER_ROOM: &[Item] = &[
    Item::wall("server_rack").w(0.8).many(2, 0.15, 12),
    Item::wall("network_cabinet").w(0.8).area(6.0),
    Item::wall("ups_unit").w(0.6).area(5.0),
    Item::wall("crac_unit").w(1.2).area(12.0),
    Item::wall("patch_panel").w(0.6).at_y(1.6).area(4.0),
    Item::corner("fire_suppression_tank"),
    Item::scatter("cable_tray").area(10.0).w(0.5),
    Item::ceiling("strip_light").many(1, 0.05, 6),
];

const RETAIL: &[Item] = &[
    Item::scatter("display_shelf").many(2, 0.25, 16).w(1.0),
    Item::wall("wall_shelving").w(1.4).many(2, 0.1, 8),
    Item::wall("checkout_counter").w(1.6),
    Item::centre("display_table").w(1.2).many(1, 0.08, 6),
    Item::wall("clothing_rack").w(1.2).area(20.0).many(1, 0.05, 6),
    Item::corner("mannequin").many(1, 0.05, 4),
    Item::wall("fitting_room").w(1.0).area(30.0),
    Item::ceiling("track_light").many(2, 0.06, 12),
];

const WAREHOUSE: &[Item] = &[
    Item::scatter("pallet_rack").many(2, 0.2, 24).w(1.4),
    Item::scatter("pallet").many(2, 0.3, 24).w(1.2),
    Item::wall("loading_dock").w(2.4).area(40.0),
    Item::corner("forklift").area(40.0),
    Item::wall("packing_station").w(1.6).area(20.0),
    Item::wall("shelving_unit").w(1.2).many(1, 0.05, 8),
    Item::ceiling("high_bay_light").many(2, 0.03, 16),
];

const CLASSROOM: &[Item] = &[
    Item::scatter("student_desk").many(6, 0.5, 30).w(0.7),
    Item::scatter("student_chair").many(6, 0.5, 30).w(0.5),
    Item::wall("teacher_desk").w(1.4),
    Item::wall("whiteboard").w(2.4).at_y(1.3),
    Item::wall("bookshelf").w(0.9).many(1, 0.04, 4),
    Item::wall("cubby_unit").w(1.2).area(20.0),
    Item::corner("globe_stand").area(15.0),
    Item::wall("noticeboard").w(1.2).at_y(1.5),
    Item::ceiling("ceiling_light").many(2, 0.04, 8),
];

const LIBRARY: &[Item] = &[
    Item::scatter("bookshelf").many(4, 0.4, 30).w(1.0),
    Item::scatter("study_table").many(1, 0.06, 8).w(1.4),
    Item::scatter("reading_chair").many(2, 0.1, 12).w(0.6),
    Item::wall("librarian_desk").w(1.8),
    Item::wall("card_catalog").w(1.0).area(20.0),
    Item::corner("floor_lamp").many(1, 0.04, 4),
    Item::corner("houseplant"),
    Item::ceiling("ceiling_light").many(2, 0.03, 10),
];

const LAB: &[Item] = &[
    Item::wall("lab_bench").w(2.0).many(2, 0.1, 8),
    Item::centre("island_bench").w(2.0).area(24.0),
    Item::wall("fume_hood").w(1.5).area(12.0),
    Item::wall("biosafety_cabinet").w(1.2).area(14.0),
    Item::wall("refrigerator").w(0.8).area(8.0),
    Item::wall("storage_cabinet").w(1.0).many(1, 0.04, 4),
    Item::wall("eyewash_station").w(0.4).at_y(1.0),
    Item::scatter("lab_stool").many(2, 0.1, 12).w(0.4),
    Item::ceiling("strip_light").many(2, 0.05, 10),
];

const MEDICAL: &[Item] = &[
    Item::centre("exam_table").w(1.9),
    Item::wall("supply_cabinet").w(1.0),
    Item::wall("sink").w(0.7),
    Item::wall("desk").w(1.2),
    Item::scatter("office_chair").many(1, 0.0, 2).w(0.6),
    Item::wall("medical_monitor").w(0.6).at_y(1.4).area(8.0),
    Item::corner("biohazard_bin"),
    Item::wall("curtain_rail").w(1.5).at_y(2.0).area(10.0),
    Item::wall("xray_viewer").w(0.7).at_y(1.5).area(9.0),
    Item::ceiling("exam_light"),
];

const WARD: &[Item] = &[
    Item::wall("hospital_bed").w(1.1).many(1, 0.06, 8),
    Item::wall("bedside_cabinet").w(0.5).many(1, 0.06, 8),
    Item::wall("iv_stand").w(0.4).many(1, 0.06, 8),
    Item::wall("vitals_monitor").w(0.5).at_y(1.3).many(1, 0.06, 8),
    Item::wall("supply_cabinet").w(1.0).area(16.0),
    Item::wall("nurse_station").w(1.6).area(24.0),
    Item::wall("curtain_rail").w(1.5).at_y(2.0),
    Item::corner("wheelchair"),
    Item::ceiling("ceiling_light").many(2, 0.03, 8),
];

const GYM: &[Item] = &[
    Item::scatter("treadmill").many(1, 0.05, 8).w(1.0),
    Item::scatter("weight_bench").many(1, 0.05, 8).w(1.2),
    Item::wall("weight_rack").w(1.6).many(1, 0.03, 4),
    Item::wall("dumbbell_rack").w(1.4).area(20.0),
    Item::centre("cable_machine").w(1.6).area(30.0),
    Item::wall("mirror_wall").w(2.4).at_y(1.2),
    Item::corner("water_cooler"),
    Item::scatter("exercise_mat").many(1, 0.05, 8).w(0.8),
    Item::ceiling("strip_light").many(2, 0.03, 10),
];

const RESTAURANT: &[Item] = &[
    Item::scatter("dining_table").many(2, 0.12, 16).w(1.0),
    Item::scatter("dining_chair").many(4, 0.4, 48).w(0.5),
    Item::wall("booth_seat").w(1.6).many(1, 0.04, 6).area(20.0),
    Item::wall("host_stand").w(0.8),
    Item::wall("service_station").w(1.2).area(16.0),
    Item::wall("bar_counter").w(2.0).area(30.0),
    Item::corner("houseplant").many(1, 0.03, 4),
    Item::ceiling("pendant_light").many(2, 0.06, 16),
];

const BAR: &[Item] = &[
    Item::wall("bar_counter").w(3.0),
    Item::wall("back_bar_shelf").w(2.4).at_y(1.4),
    Item::scatter("bar_stool").many(3, 0.3, 16).w(0.5),
    Item::scatter("pub_table").many(1, 0.08, 8).w(0.8),
    Item::wall("booth_seat").w(1.6).many(1, 0.04, 6).area(20.0),
    Item::wall("beer_tap").w(0.6).at_y(1.0),
    Item::wall("dartboard").w(0.5).at_y(1.7).area(16.0),
    Item::corner("jukebox").area(20.0),
    Item::ceiling("pendant_light").many(2, 0.05, 12),
];

const CELL: &[Item] = &[
    Item::wall("bunk").w(1.9),
    Item::wall("toilet").w(0.6),
    Item::wall("sink").w(0.5),
    Item::wall("shelf").w(0.8).at_y(1.5),
    Item::corner("stool"),
];

const GENERIC: &[Item] = &[
    Item::wall("table").w(1.4).area(6.0),
    Item::scatter("chair").many(1, 0.06, 6).w(0.5),
    Item::wall("shelving_unit").w(1.0).area(5.0),
    Item::corner("houseplant").area(6.0),
    Item::ceiling("ceiling_light").many(1, 0.04, 4),
];
