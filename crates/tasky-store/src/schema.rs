//! Diesel table definitions. Keep in sync with `migrations/`.

diesel::table! {
    projects (id) {
        id -> Text,
        slug -> Text,
        name -> Text,
        repo_path -> Nullable<Text>,
        repo_url -> Nullable<Text>,
        created_at -> Text,
    }
}

diesel::table! {
    goals (id) {
        id -> Text,
        project_id -> Text,
        slug -> Text,
        title -> Text,
        description -> Text,
        spec -> Nullable<Text>,
        status -> Text,
        created_at -> Text,
        updated_at -> Text,
        completed_at -> Nullable<Text>,
    }
}

diesel::table! {
    tasks (id) {
        id -> Text,
        goal_id -> Text,
        title -> Text,
        body -> Text,
        test_plan -> Text,
        pr -> Nullable<Text>,
        status -> Text,
        created_at -> Text,
        updated_at -> Text,
        completed_at -> Nullable<Text>,
    }
}

diesel::table! {
    task_dependencies (task_id, depends_on_id) {
        task_id -> Text,
        depends_on_id -> Text,
    }
}

diesel::table! {
    task_links (id) {
        id -> Text,
        task_id -> Text,
        kind -> Text,
        reference -> Text,
        created_at -> Text,
    }
}

diesel::joinable!(goals -> projects (project_id));
diesel::joinable!(tasks -> goals (goal_id));
diesel::joinable!(task_links -> tasks (task_id));

diesel::allow_tables_to_appear_in_same_query!(
    projects,
    goals,
    tasks,
    task_dependencies,
    task_links,
);
