//! Stage Manager's scenes, as StageManagerForWindows keeps them (MIT, depoledna / Andreas
//! Wäscher: `SceneManager`, `Scene`, `MainWindow.SyncVisibilityByUpdatedTimeStamp`): the windows
//! of one app are one scene; one scene is on stage, the others wait in the sidebar, most recently
//! used first and at most five of them, unless an app's icon filters the sidebar down to that app.
//! A window can be dragged out of a scene into one of its own, or pulled from a scene into the
//! one on stage. Portable and tested; stage.rs puts it on the screen.

/// How many scenes the sidebar shows (`MainWindow.MAX_SCENES`).
pub const MAX_SCENES: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scene {
    pub id: u64,
    /// the app whose windows these are (`GetWindowGroupKey`)
    pub app: String,
    /// its windows, the first in front
    pub windows: Vec<isize>,
    /// when it was last on stage (a counter: larger is more recent)
    pub updated: u64,
}

#[derive(Debug, Default)]
pub struct Scenes {
    scenes: Vec<Scene>,
    current: Option<u64>,
    next_id: u64,
    clock: u64,
    /// the app the sidebar is filtered to (a click on its icon)
    filter: Option<String>,
}

/// What a switch does: the windows that leave the stage, and those that come onto it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Switch {
    pub outgoing: Vec<isize>,
    pub incoming: Vec<isize>,
}

impl Scenes {
    pub fn new() -> Scenes {
        Scenes { next_id: 1, ..Default::default() }
    }

    pub fn all(&self) -> &[Scene] {
        &self.scenes
    }

    pub fn get(&self, id: u64) -> Option<&Scene> {
        self.scenes.iter().find(|s| s.id == id)
    }

    pub fn current(&self) -> Option<u64> {
        self.current
    }

    pub fn current_scene(&self) -> Option<&Scene> {
        self.current.and_then(|id| self.get(id))
    }

    pub fn is_current_window(&self, hwnd: isize) -> bool {
        self.current_scene().is_some_and(|s| s.windows.contains(&hwnd))
    }

    /// The scene `hwnd` is in (`FindSceneForWindow`).
    pub fn scene_of(&self, hwnd: isize) -> Option<u64> {
        self.scenes.iter().find(|s| s.windows.contains(&hwnd)).map(|s| s.id)
    }

    fn scene_of_app(&self, app: &str) -> Option<u64> {
        self.scenes.iter().find(|s| s.app == app).map(|s| s.id)
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    fn new_scene(&mut self, app: &str, windows: Vec<isize>) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        // a new scene has never been on stage: older than any that has
        self.scenes.push(Scene { id, app: app.to_string(), windows, updated: 0 });
        id
    }

    /// A window of `app` appeared (`SwitchToSceneByNewWindow`): it joins its app's scene, or
    /// starts one. Returns its scene and whether that scene is new.
    pub fn add(&mut self, hwnd: isize, app: &str) -> (u64, bool) {
        if let Some(id) = self.scene_of(hwnd) {
            return (id, false);
        }
        match self.scene_of_app(app) {
            Some(id) => {
                let s = self.scenes.iter_mut().find(|s| s.id == id).unwrap();
                s.windows.insert(0, hwnd);
                (id, false)
            }
            None => (self.new_scene(app, vec![hwnd]), true),
        }
    }

    /// A window is gone (`WindowDestroyed`). Returns the scene that comes on stage when it was
    /// the current scene's last window (the most recently used of the others).
    pub fn remove(&mut self, hwnd: isize) -> Option<u64> {
        let id = self.scene_of(hwnd)?;
        let s = self.scenes.iter_mut().find(|s| s.id == id).unwrap();
        s.windows.retain(|w| *w != hwnd);
        if !s.windows.is_empty() {
            return None;
        }
        self.scenes.retain(|s| s.id != id);
        if self.current != Some(id) {
            return None;
        }
        self.current = None;
        self.scenes.iter().max_by_key(|s| s.updated).map(|s| s.id)
    }

    /// Scene `id` comes on stage (`SwitchTo`): what leaves and what comes. Switching to the scene
    /// already there does nothing.
    pub fn switch_to(&mut self, id: u64) -> Option<Switch> {
        if self.current == Some(id) || self.get(id).is_none() {
            return None;
        }
        let outgoing = self.current_scene().map(|s| s.windows.clone()).unwrap_or_default();
        let t = self.tick();
        if let Some(c) = self.current.and_then(|c| self.scenes.iter_mut().find(|s| s.id == c)) {
            // the scene leaving is the most recent in the sidebar
            c.updated = t;
        }
        let t = self.tick();
        let s = self.scenes.iter_mut().find(|s| s.id == id).unwrap();
        s.updated = t;
        let incoming = s.windows.clone();
        self.current = Some(id);
        Some(Switch { outgoing, incoming })
    }

    /// `hwnd` into scene `to` (`MoveWindow`): from the sidebar into the one on stage, say. A scene
    /// left empty goes.
    pub fn move_window(&mut self, hwnd: isize, to: u64) -> bool {
        let Some(from) = self.scene_of(hwnd) else { return false };
        if from == to || self.get(to).is_none() {
            return false;
        }
        if let Some(s) = self.scenes.iter_mut().find(|s| s.id == from) {
            s.windows.retain(|w| *w != hwnd);
        }
        if let Some(s) = self.scenes.iter_mut().find(|s| s.id == to) {
            s.windows.insert(0, hwnd);
        }
        self.scenes.retain(|s| !s.windows.is_empty());
        true
    }

    /// The last window of sidebar scene `id` comes into the scene on stage (`PopWindowFrom`).
    pub fn pop_from(&mut self, id: u64) -> Option<isize> {
        let cur = self.current?;
        if cur == id {
            return None;
        }
        let w = *self.get(id)?.windows.last()?;
        self.move_window(w, cur).then_some(w)
    }

    /// A window of the scene on stage, dragged into the sidebar, becomes a scene of its own
    /// (`SeparateWindowToNewScene`): not the scene's only window.
    pub fn separate(&mut self, hwnd: isize) -> Option<u64> {
        let cur = self.current?;
        let s = self.get(cur)?;
        if !s.windows.contains(&hwnd) || s.windows.len() <= 1 {
            return None;
        }
        let app = s.app.clone();
        if let Some(s) = self.scenes.iter_mut().find(|s| s.id == cur) {
            s.windows.retain(|w| *w != hwnd);
        }
        let id = self.new_scene(&app, vec![hwnd]);
        // it has just been on stage: first in the sidebar
        let t = self.tick();
        if let Some(s) = self.scenes.iter_mut().find(|s| s.id == id) {
            s.updated = t;
        }
        Some(id)
    }

    /// The sidebar's scenes, top to bottom: the most recently used first, at most
    /// [`MAX_SCENES`]; filtered to one app, all of that app's.
    pub fn sidebar(&self) -> Vec<u64> {
        let mut v: Vec<&Scene> = self.scenes.iter().filter(|s| Some(s.id) != self.current).collect();
        v.sort_by_key(|s| std::cmp::Reverse(s.updated));
        match &self.filter {
            Some(app) => v.into_iter().filter(|s| &s.app == app).map(|s| s.id).collect(),
            None => v.into_iter().take(MAX_SCENES).map(|s| s.id).collect(),
        }
    }

    /// A click on an app's icon: the sidebar shows only that app's scenes, or all again
    /// (`ToggleAppFilter`).
    pub fn toggle_filter(&mut self, app: &str) {
        self.filter = if self.filter.as_deref() == Some(app) { None } else { Some(app.to_string()) };
    }

    pub fn clear_filter(&mut self) -> bool {
        self.filter.take().is_some()
    }

    pub fn filter(&self) -> Option<&str> {
        self.filter.as_deref()
    }

    /// Every window, for Stage Manager turned off.
    pub fn clear(&mut self) -> Vec<isize> {
        let all = self.scenes.iter().flat_map(|s| s.windows.iter().copied()).collect();
        *self = Scenes::new();
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_apps_windows_are_one_scene_and_a_switch_trades_them() {
        let mut s = Scenes::new();
        let (notes, new) = s.add(1, "notes");
        assert!(new);
        assert_eq!(s.add(2, "notes"), (notes, false), "the second window joins its app");
        let (calc, _) = s.add(3, "calculator");
        assert_eq!(s.switch_to(notes), Some(Switch { outgoing: vec![], incoming: vec![2, 1] }));
        assert_eq!(s.switch_to(notes), None, "already there");
        assert_eq!(s.sidebar(), vec![calc]);
        assert_eq!(s.switch_to(calc), Some(Switch { outgoing: vec![2, 1], incoming: vec![3] }));
        assert_eq!(s.sidebar(), vec![notes]);
        assert!(s.is_current_window(3));
    }

    #[test]
    fn the_sidebar_is_most_recent_first_and_keeps_five() {
        let mut s = Scenes::new();
        let ids: Vec<u64> = (0..8).map(|i| s.add(i, &format!("app{i}")).0).collect();
        for &id in &ids {
            s.switch_to(id);
        }
        // app7 on stage; the others most recent first, five of them
        assert_eq!(s.sidebar(), vec![ids[6], ids[5], ids[4], ids[3], ids[2]]);
        s.switch_to(ids[0]);
        assert_eq!(s.sidebar()[0], ids[7], "the scene that left is first");
    }

    #[test]
    fn closing_the_last_window_on_stage_brings_the_most_recent_scene() {
        let mut s = Scenes::new();
        let (a, _) = s.add(1, "a");
        let (b, _) = s.add(2, "b");
        let (c, _) = s.add(3, "c");
        s.switch_to(a);
        s.switch_to(b);
        s.switch_to(c);
        assert_eq!(s.remove(3), Some(b));
        assert_eq!(s.current(), None);
        assert_eq!(s.remove(1), None, "a sidebar scene going changes nothing on stage");
        assert!(s.get(a).is_none());
    }

    #[test]
    fn a_window_dragged_out_and_one_pulled_in() {
        let mut s = Scenes::new();
        let (notes, _) = s.add(1, "notes");
        s.add(2, "notes");
        let (calc, _) = s.add(3, "calculator");
        s.switch_to(notes);
        assert_eq!(s.separate(3), None, "not on stage");
        let own = s.separate(2).unwrap();
        assert_eq!(s.get(notes).unwrap().windows, vec![1]);
        assert_eq!(s.sidebar()[0], own, "first in the sidebar");
        assert_eq!(s.separate(1), None, "a scene's only window stays");
        // pulled from the sidebar into the scene on stage
        assert_eq!(s.pop_from(calc), Some(3));
        assert_eq!(s.get(notes).unwrap().windows, vec![3, 1]);
        assert!(s.get(calc).is_none(), "an empty scene goes");
    }

    #[test]
    fn an_icon_filters_the_sidebar_to_its_app() {
        let mut s = Scenes::new();
        let (a1, _) = s.add(1, "notes");
        s.add(2, "notes");
        let (b, _) = s.add(3, "calc");
        let (c, _) = s.add(4, "mail");
        s.switch_to(c);
        s.switch_to(a1);
        let own = {
            s.separate(2).unwrap()
        };
        s.switch_to(b);
        s.toggle_filter("notes");
        let side = s.sidebar();
        assert!(side.contains(&a1) && side.contains(&own) && side.len() == 2);
        s.toggle_filter("notes");
        assert_eq!(s.sidebar().len(), 3);
        s.toggle_filter("mail");
        assert!(s.clear_filter() && s.filter().is_none());
        assert_eq!(s.clear().len(), 4);
        assert!(s.all().is_empty());
    }
}
