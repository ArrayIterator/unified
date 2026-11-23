//! Academic - in-memory store, GPA, standing and schedules.
//!
//! Part of the `academic` module split; see `src/modules/academic.rs`.

use unified_core::hooks::HookBus;
use unified_core::json;
use dashmap::DashMap;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use std::sync::RwLock;

use super::constants::DEFAULT_APPEAL_STATUSES;
use super::constants::DEFAULT_ATTENDANCE_STATUSES;
use super::constants::DEFAULT_CALENDAR_EVENT_TYPES;
use super::constants::DEFAULT_COMPONENT_KINDS;
use super::constants::DEFAULT_ENROLLMENT_STATUSES;
use super::constants::DEFAULT_ENROLLMENT_TRANSITIONS;
use super::constants::DEFAULT_GRADE_SCALE;
use super::constants::DEFAULT_LECTURER_STATUSES;
use super::constants::DEFAULT_MAX_CREDITS_PER_SEMESTER;
use super::constants::DEFAULT_PASSING_POINTS;
use super::constants::DEFAULT_STANDING_RULES;
use super::constants::DEFAULT_STUDENT_STATUSES;
use super::constants::DEFAULT_THESIS_STATUSES;
use super::constants::DEFAULT_THESIS_TRANSITIONS;
use super::constants::KIND_APPEAL_STATUS;
use super::constants::KIND_ATTENDANCE_STATUS;
use super::constants::KIND_CALENDAR_EVENT_TYPE;
use super::constants::KIND_COMPONENT_KIND;
use super::constants::KIND_ENROLLMENT_STATUS;
use super::constants::KIND_ENROLLMENT_TRANSITION;
use super::constants::KIND_GRADE_SCALE;
use super::constants::KIND_LECTURER_STATUS;
use super::constants::KIND_STANDING_RULE;
use super::constants::KIND_STUDENT_STATUS;
use super::constants::KIND_THESIS_STATUS;
use super::constants::KIND_THESIS_TRANSITION;
use super::constants::SETTING_DEFAULT_MAX_CREDITS;
use super::constants::SETTING_DEFAULT_PASSING_POINTS;
use super::helpers::time_to_minutes;
use super::types::AcademicDefinition;
use super::types::AcademicSetting;
use super::types::Attendance;
use super::types::CalendarEvent;
use super::types::ComponentGrade;
use super::types::Enrollment;
use super::types::FinalGrade;
use super::types::Grade;
use super::types::GradeAppeal;
use super::types::GradeComponent;
use super::types::Lecturer;
use super::types::Program;
use super::types::Room;
use super::types::Schedule;
use super::types::Semester;
use super::types::StandingRecord;
use super::types::Student;
use super::types::Subject;
use super::types::Thesis;
use super::types::Transcript;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// In-memory store. The migration tables are the persistence
/// targets; this store is the live read path (the same split the
/// E-Learning module uses).
#[derive(Debug)]
pub struct AcademicState {
    pub(crate) hooks: RwLock<Arc<HookBus>>,
    pub(crate) programs: DashMap<String, Program>,
    pub(crate) students: DashMap<String, Student>,
    pub(crate) lecturers: DashMap<String, Lecturer>,
    pub(crate) subjects: DashMap<String, Subject>,
    pub(crate) rooms: DashMap<String, Room>,
    pub(crate) semesters: DashMap<String, Semester>,
    pub(crate) calendar_events: DashMap<String, CalendarEvent>,
    pub(crate) schedules: DashMap<String, Schedule>,
    pub(crate) enrollments: DashMap<String, Enrollment>,
    pub(crate) attendance: DashMap<String, Attendance>,
    pub(crate) grade_components: DashMap<String, GradeComponent>,
    pub(crate) grades: DashMap<String, Grade>,
    pub(crate) grade_appeals: DashMap<String, GradeAppeal>,
    pub(crate) transcripts: DashMap<String, Transcript>,
    pub(crate) theses: DashMap<String, Thesis>,
    pub(crate) standing_records: DashMap<String, StandingRecord>,
    pub(crate) definitions: DashMap<String, AcademicDefinition>,
    pub(crate) settings: DashMap<String, AcademicSetting>,
}

impl AcademicState {
    pub(crate) fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            programs: DashMap::new(),
            students: DashMap::new(),
            lecturers: DashMap::new(),
            subjects: DashMap::new(),
            rooms: DashMap::new(),
            semesters: DashMap::new(),
            calendar_events: DashMap::new(),
            schedules: DashMap::new(),
            enrollments: DashMap::new(),
            attendance: DashMap::new(),
            grade_components: DashMap::new(),
            grades: DashMap::new(),
            grade_appeals: DashMap::new(),
            transcripts: DashMap::new(),
            theses: DashMap::new(),
            standing_records: DashMap::new(),
            definitions: DashMap::new(),
            settings: DashMap::new(),
        }
    }

    pub(crate) fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    pub(crate) fn notify(&self, user_id: &str, title: &str, body: &str) {
        self.emit(
            "notification.requested",
            json!({
                "user_id": user_id,
                "title": title,
                "body": body,
                "kind": "academic",
            }),
        );
    }

    pub(crate) fn student_by_user(&self, user_id: &str) -> Option<Student> {
        self.students
            .iter()
            .find(|entry| entry.value().user_id == user_id)
            .map(|entry| entry.value().clone())
    }

    pub(crate) fn lecturer_by_user(&self, user_id: &str) -> Option<Lecturer> {
        self.lecturers
            .iter()
            .find(|entry| entry.value().user_id == user_id)
            .map(|entry| entry.value().clone())
    }

    /// Ask the E-Learning module whether a course exists
    /// (`elearning.course.requested`). Returns the answering result
    /// when one arrives, or `None` when nothing answers (no handler
    /// exists in this tree yet — see the module header).
    pub(crate) fn elearning_course(&self, course_id: &str) -> Option<Value> {
        let context = self.bus().emit(
            "elearning.course.requested",
            json!({ "course_id": course_id }),
        );
        context.results.into_iter().find(|result| {
            result.get("course_id").and_then(|value| value.as_str()) == Some(course_id)
        })
    }

    /// True when the e-learning hook answers that the course exists.
    pub(crate) fn elearning_course_exists(&self, course_id: &str) -> bool {
        self.elearning_course(course_id)
            .and_then(|result| result.get("exists").and_then(|value| value.as_bool()))
            .unwrap_or(false)
    }

    /// Active definitions of one kind, in sort order.
    pub(crate) fn definitions_for(&self, kind: &str) -> Vec<AcademicDefinition> {
        let mut items: Vec<AcademicDefinition> = self
            .definitions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|definition| definition.kind == kind && definition.active)
            .collect();
        items.sort_by(|a, b| {
            a.sort_order
                .cmp(&b.sort_order)
                .then_with(|| a.key.cmp(&b.key))
        });
        items
    }

    /// True when `value` is an allowed key for `kind`: an active
    /// definition key when definitions exist, otherwise one of the
    /// built-in configurable defaults for that kind.
    pub(crate) fn valid_status(&self, kind: &str, value: &str) -> bool {
        let defined = self.definitions_for(kind);
        if !defined.is_empty() {
            return defined.iter().any(|definition| definition.key == value);
        }
        match kind {
            KIND_STUDENT_STATUS => DEFAULT_STUDENT_STATUSES.contains(&value),
            KIND_LECTURER_STATUS => DEFAULT_LECTURER_STATUSES.contains(&value),
            KIND_ENROLLMENT_STATUS => DEFAULT_ENROLLMENT_STATUSES.contains(&value),
            KIND_ATTENDANCE_STATUS => DEFAULT_ATTENDANCE_STATUSES.contains(&value),
            KIND_COMPONENT_KIND => DEFAULT_COMPONENT_KINDS.contains(&value),
            KIND_THESIS_STATUS => DEFAULT_THESIS_STATUSES.contains(&value),
            KIND_APPEAL_STATUS => DEFAULT_APPEAL_STATUSES.contains(&value),
            KIND_CALENDAR_EVENT_TYPE => DEFAULT_CALENDAR_EVENT_TYPES.contains(&value),
            _ => false,
        }
    }

    /// The grade scale in force: admin definitions when any exist,
    /// otherwise the configurable default scale. Each entry is
    /// (letter, minimum score, points), ordered by minimum score
    /// descending.
    pub(crate) fn grade_scale(&self) -> Vec<(String, f64, f64)> {
        let defined = self.definitions_for(KIND_GRADE_SCALE);
        if !defined.is_empty() {
            let mut scale: Vec<(String, f64, f64)> = defined
                .iter()
                .map(|definition| {
                    let min = definition
                        .config
                        .get("min_score")
                        .and_then(|value| value.as_f64())
                        .unwrap_or(0.0);
                    let points = definition
                        .config
                        .get("points")
                        .and_then(|value| value.as_f64())
                        .unwrap_or(0.0);
                    (definition.key.clone(), min, points)
                })
                .collect();
            scale.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            return scale;
        }
        DEFAULT_GRADE_SCALE
            .iter()
            .map(|(letter, min, points)| (letter.to_string(), *min, *points))
            .collect()
    }

    /// The standing rules in force: admin definitions when any
    /// exist, otherwise the configurable default rules. Each entry
    /// is (standing key, minimum GPA); bands are half-open (see
    /// `standing_for_gpa`), so only the minimum is stored.
    pub(crate) fn standing_rules(&self) -> Vec<(String, f64)> {
        let defined = self.definitions_for(KIND_STANDING_RULE);
        if !defined.is_empty() {
            return defined
                .iter()
                .map(|definition| {
                    let min = definition
                        .config
                        .get("min_gpa")
                        .and_then(|value| value.as_f64())
                        .unwrap_or(0.0);
                    (definition.key.clone(), min)
                })
                .collect();
        }
        DEFAULT_STANDING_RULES
            .iter()
            .map(|(key, min)| (key.to_string(), *min))
            .collect()
    }

    /// Allowed state transitions for a kind: admin `*_transition`
    /// definitions when any exist, otherwise the configurable
    /// default table.
    pub(crate) fn allowed_transitions(&self, kind: &str) -> Vec<(String, String)> {
        let defined = self.definitions_for(kind);
        if !defined.is_empty() {
            return defined
                .iter()
                .filter_map(|definition| {
                    let from = definition
                        .config
                        .get("from")
                        .and_then(|value| value.as_str())?
                        .to_string();
                    let to = definition
                        .config
                        .get("to")
                        .and_then(|value| value.as_str())?
                        .to_string();
                    Some((from, to))
                })
                .collect();
        }
        let table = match kind {
            KIND_THESIS_TRANSITION => &DEFAULT_THESIS_TRANSITIONS[..],
            KIND_ENROLLMENT_TRANSITION => &DEFAULT_ENROLLMENT_TRANSITIONS[..],
            _ => &[][..],
        };
        table
            .iter()
            .map(|(from, to)| (from.to_string(), to.to_string()))
            .collect()
    }

    /// The default maximum credits per semester: the setting when
    /// set, otherwise the configurable default.
    pub(crate) fn default_max_credits(&self) -> f64 {
        self.settings
            .get(SETTING_DEFAULT_MAX_CREDITS)
            .and_then(|entry| entry.value().value.as_f64())
            .filter(|value| value.is_finite() && *value > 0.0)
            .unwrap_or(DEFAULT_MAX_CREDITS_PER_SEMESTER)
    }

    /// The passing grade points: the setting when set, otherwise
    /// the configurable default.
    pub(crate) fn passing_points(&self) -> f64 {
        self.settings
            .get(SETTING_DEFAULT_PASSING_POINTS)
            .and_then(|entry| entry.value().value.as_f64())
            .filter(|value| value.is_finite() && *value >= 0.0)
            .unwrap_or(DEFAULT_PASSING_POINTS)
    }

    /// The credit cap for one program: the program value when set,
    /// otherwise the default maximum credits.
    pub(crate) fn credit_cap_for(&self, program_id: &str) -> f64 {
        self.programs
            .get(program_id)
            .and_then(|entry| entry.value().max_credits_per_semester)
            .filter(|value| value.is_finite() && *value > 0.0)
            .unwrap_or_else(|| self.default_max_credits())
    }

    /// Credits already registered (requested or approved) by one
    /// student in one semester.
    pub(crate) fn registered_credits(&self, student_id: &str, semester_id: &str) -> f64 {
        self.enrollments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|enrollment| {
                enrollment.student_id == student_id
                    && enrollment.semester_id == semester_id
                    && matches!(enrollment.status.as_str(), "requested" | "approved")
            })
            .map(|enrollment| enrollment.credits)
            .sum()
    }

    /// The final grade of one enrollment: the weight-normalized
    /// score over the components that have grades, with the letter
    /// and points from the grade scale in force. `None` scores mean
    /// no component has been graded yet.
    pub(crate) fn final_grade(&self, enrollment: &Enrollment) -> FinalGrade {
        let components: Vec<GradeComponent> = self
            .grade_components
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|component| component.subject_id == enrollment.subject_id)
            .collect();
        let mut component_grades: Vec<ComponentGrade> = Vec::new();
        let mut weighted_sum = 0.0;
        let mut weight_total = 0.0;
        for component in &components {
            let grade = self
                .grades
                .iter()
                .map(|entry| entry.value().clone())
                .find(|grade| {
                    grade.enrollment_id == enrollment.id && grade.component_id == component.id
                });
            if let Some(grade) = &grade {
                weighted_sum += grade.score * component.weight_percent;
                weight_total += component.weight_percent;
            }
            component_grades.push(ComponentGrade {
                component_id: component.id.clone(),
                component_name: component.name.clone(),
                kind: component.kind.clone(),
                weight_percent: component.weight_percent,
                score: grade.map(|grade| grade.score),
            });
        }
        let weighted_score = if weight_total > 0.0 {
            Some((weighted_sum / weight_total * 100.0).round() / 100.0)
        } else {
            None
        };
        let (letter, points) = match weighted_score {
            Some(score) => {
                let scale = self.grade_scale();
                let found = scale
                    .iter()
                    .find(|(_, min, _)| score >= *min)
                    .map(|(letter, _, points)| (Some(letter.clone()), Some(*points)));
                found.unwrap_or((None, None))
            }
            None => (None, None),
        };
        FinalGrade {
            enrollment_id: enrollment.id.clone(),
            student_id: enrollment.student_id.clone(),
            subject_id: enrollment.subject_id.clone(),
            semester_id: enrollment.semester_id.clone(),
            credits: enrollment.credits,
            weighted_score,
            letter,
            points,
            components: component_grades,
        }
    }

    /// Cumulative GPA over every enrollment of one student that has
    /// a final grade, plus earned credits (credits whose points
    /// reach the passing threshold) and total graded credits.
    pub(crate) fn gpa_for(&self, student_id: &str) -> (f64, f64, f64) {
        let passing = self.passing_points();
        let mut points_sum = 0.0;
        let mut credits_sum = 0.0;
        let mut earned = 0.0;
        for entry in self.enrollments.iter() {
            let enrollment = entry.value();
            if enrollment.student_id != student_id {
                continue;
            }
            let grade = self.final_grade(enrollment);
            if let Some(points) = grade.points {
                points_sum += points * enrollment.credits;
                credits_sum += enrollment.credits;
                if points >= passing {
                    earned += enrollment.credits;
                }
            }
        }
        let gpa = if credits_sum > 0.0 {
            (points_sum / credits_sum * 100.0).round() / 100.0
        } else {
            0.0
        };
        (gpa, credits_sum, earned)
    }

    /// The standing key for a GPA, from the standing rules in force.
    /// Bands are half-open: rules are tried in descending minimum
    /// GPA order and the first rule whose minimum the GPA reaches
    /// wins, so an unrounded GPA between two band edges (for
    /// example 2.49995) still lands in exactly one band instead of
    /// falling through every band.
    pub(crate) fn standing_for_gpa(&self, gpa: f64) -> String {
        let mut rules = self.standing_rules();
        rules.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        for (key, min) in rules {
            if gpa >= min {
                return key;
            }
        }
        "good".to_string()
    }

    /// True when two weekly sessions overlap (same day, intersecting
    /// time ranges).
    pub(crate) fn sessions_overlap(
        a_day: i32,
        a_start: i32,
        a_end: i32,
        b_day: i32,
        b_start: i32,
        b_end: i32,
    ) -> bool {
        a_day == b_day && a_start < b_end && b_start < a_end
    }

    /// Find a conflicting schedule for a new or updated session:
    /// same room or same lecturer, same semester, overlapping time.
    pub(crate) fn schedule_conflict(
        &self,
        semester_id: &str,
        room_id: &str,
        lecturer_id: &str,
        day_of_week: i32,
        start_minutes: i32,
        end_minutes: i32,
        exclude_id: Option<&str>,
    ) -> Option<Schedule> {
        self.schedules
            .iter()
            .map(|entry| entry.value().clone())
            .find(|schedule| {
                schedule.semester_id == semester_id
                    && schedule.status == "active"
                    && exclude_id != Some(schedule.id.as_str())
                    && (schedule.room_id == room_id || schedule.lecturer_id == lecturer_id)
                    && Self::sessions_overlap(
                        schedule.day_of_week,
                        time_to_minutes(&schedule.start_time).unwrap_or(0),
                        time_to_minutes(&schedule.end_time).unwrap_or(0),
                        day_of_week,
                        start_minutes,
                        end_minutes,
                    )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_overlap_only_on_same_day_with_intersecting_times() {
        assert!(AcademicState::sessions_overlap(1, 540, 600, 1, 570, 630));
        assert!(!AcademicState::sessions_overlap(1, 540, 600, 1, 600, 660));
        assert!(!AcademicState::sessions_overlap(1, 540, 600, 2, 540, 600));
        assert!(!AcademicState::sessions_overlap(1, 540, 600, 1, 660, 720));
    }

    #[test]
    fn standing_for_gpa_uses_default_rules() {
        let state = AcademicState::new();
        assert_eq!(state.standing_for_gpa(3.9), "good");
        assert_eq!(state.standing_for_gpa(2.5), "good");
        assert_eq!(state.standing_for_gpa(2.2), "warning");
        assert_eq!(state.standing_for_gpa(1.5), "probation");
        assert_eq!(state.standing_for_gpa(0.5), "dismissed");
    }

    #[test]
    fn standing_for_gpa_has_no_gaps_for_unrounded_gpas() {
        // Half-open bands: an unrounded GPA just below a band edge
        // must land in the lower band, never fall through all bands
        // to the "good" fallback.
        let state = AcademicState::new();
        assert_eq!(state.standing_for_gpa(2.49995), "warning");
        assert_eq!(state.standing_for_gpa(2.4999), "warning");
        assert_eq!(state.standing_for_gpa(1.99995), "probation");
        assert_eq!(state.standing_for_gpa(1.9999), "probation");
        assert_eq!(state.standing_for_gpa(0.99995), "dismissed");
        assert_eq!(state.standing_for_gpa(0.9999), "dismissed");
        assert_eq!(state.standing_for_gpa(4.0), "good");
        assert_eq!(state.standing_for_gpa(0.0), "dismissed");
    }

    #[test]
    fn standing_rules_default_to_half_open_min_only_bands() {
        let state = AcademicState::new();
        assert_eq!(
            state.standing_rules(),
            vec![
                ("good".to_string(), 2.5),
                ("warning".to_string(), 2.0),
                ("probation".to_string(), 1.0),
                ("dismissed".to_string(), 0.0),
            ]
        );
    }

    #[test]
    fn grade_scale_defaults_to_a_through_f() {
        let state = AcademicState::new();
        let scale = state.grade_scale();
        assert_eq!(scale.len(), 5);
        assert_eq!(scale[0], ("A".to_string(), 90.0, 4.0));
        assert_eq!(scale[4], ("F".to_string(), 0.0, 0.0));
    }

    #[test]
    fn valid_status_uses_default_lists() {
        let state = AcademicState::new();
        assert!(state.valid_status("student_status", "active"));
        assert!(state.valid_status("student_status", "graduated"));
        assert!(!state.valid_status("student_status", "bogus"));
        assert!(!state.valid_status("unknown_kind", "active"));
    }

    #[test]
    fn default_credit_cap_and_passing_points() {
        let state = AcademicState::new();
        assert_eq!(state.default_max_credits(), 24.0);
        assert_eq!(state.passing_points(), 2.0);
    }
}
