use std::path::{Path, PathBuf};

use call::ActiveCall;
use git::status::{FileStatus, StatusCode, TrackedStatus};
use git_ui::project_diff::ProjectDiff;
use gpui::{AppContext as _, BackgroundExecutor, TestAppContext, VisualTestContext};
use project::ProjectPath;
use serde_json::json;
use util::{path, rel_path::rel_path};
use workspace::{MultiWorkspace, Workspace};

//
use crate::TestServer;

#[gpui::test]
async fn test_project_diff(cx_a: &mut TestAppContext, cx_b: &mut TestAppContext) {
    let mut server = TestServer::start(cx_a.background_executor.clone()).await;
    let client_a = server.create_client(cx_a, "user_a").await;
    let client_b = server.create_client(cx_b, "user_b").await;
    cx_a.set_name("cx_a");
    cx_b.set_name("cx_b");

    server
        .create_room(&mut [(&client_a, cx_a), (&client_b, cx_b)])
        .await;

    client_a
        .fs()
        .insert_tree(
            path!("/a"),
            json!({
                ".git": {},
                "changed.txt": "after\n",
                "unchanged.txt": "unchanged\n",
                "created.txt": "created\n",
                "secret.pem": "secret-changed\n",
            }),
        )
        .await;

    client_a.fs().set_head_and_index_for_repo(
        Path::new(path!("/a/.git")),
        &[
            ("changed.txt", "before\n".to_string()),
            ("unchanged.txt", "unchanged\n".to_string()),
            ("deleted.txt", "deleted\n".to_string()),
            ("secret.pem", "shh\n".to_string()),
        ],
    );
    let (project_a, worktree_id) = client_a.build_local_project(path!("/a"), cx_a).await;
    let active_call_a = cx_a.read(ActiveCall::global);
    let project_id = active_call_a
        .update(cx_a, |call, cx| call.share_project(project_a.clone(), cx))
        .await
        .unwrap();

    cx_b.update(editor::init);
    cx_b.update(git_ui::init);
    let project_b = client_b.join_remote_project(project_id, cx_b).await;
    let window_b = cx_b.add_window(|window, cx| {
        let workspace = cx.new(|cx| {
            Workspace::new(
                None,
                project_b.clone(),
                client_b.app_state.clone(),
                window,
                cx,
            )
        });
        MultiWorkspace::new(workspace, cx)
    });
    let cx_b = &mut VisualTestContext::from_window(*window_b, cx_b);
    let workspace_b = window_b
        .root(cx_b)
        .unwrap()
        .read_with(cx_b, |multi_workspace, _| {
            multi_workspace.workspace().clone()
        });

    cx_b.update(|window, cx| {
        window
            .focused(cx)
            .unwrap()
            .dispatch_action(&git_ui::project_diff::Diff, window, cx)
    });
    let diff = workspace_b.update(cx_b, |workspace, cx| {
        workspace.active_item(cx).unwrap().act_as::<ProjectDiff>(cx)
    });
    let diff = diff.unwrap();
    cx_b.run_until_parked();

    diff.update(cx_b, |diff, cx| {
        assert_eq!(
            diff.excerpt_paths(cx),
            vec![
                rel_path("changed.txt").into_arc(),
                rel_path("deleted.txt").into_arc(),
                rel_path("created.txt").into_arc()
            ]
        );
    });

    client_a
        .fs()
        .insert_tree(
            path!("/a"),
            json!({
                ".git": {},
                "changed.txt": "before\n",
                "unchanged.txt": "changed\n",
                "created.txt": "created\n",
                "secret.pem": "secret-changed\n",
            }),
        )
        .await;
    cx_b.run_until_parked();

    project_b.update(cx_b, |project, cx| {
        let project_path = ProjectPath {
            worktree_id,
            path: rel_path("unchanged.txt").into(),
        };
        let status = project.project_path_git_status(&project_path, cx);
        assert_eq!(
            status.unwrap(),
            FileStatus::Tracked(TrackedStatus {
                worktree_status: StatusCode::Modified,
                index_status: StatusCode::Unmodified,
            })
        );
    });

    diff.update(cx_b, |diff, cx| {
        assert_eq!(
            diff.excerpt_paths(cx),
            vec![
                rel_path("deleted.txt").into_arc(),
                rel_path("unchanged.txt").into_arc(),
                rel_path("created.txt").into_arc()
            ]
        );
    });
}

#[gpui::test]
async fn test_repository_remove_worktree_remote_roundtrip(
    executor: BackgroundExecutor,
    cx_a: &mut TestAppContext,
    cx_b: &mut TestAppContext,
) {
    let mut server = TestServer::start(executor.clone()).await;
    let client_a = server.create_client(cx_a, "user_a").await;
    let client_b = server.create_client(cx_b, "user_b").await;
    server
        .create_room(&mut [(&client_a, cx_a), (&client_b, cx_b)])
        .await;
    let active_call_a = cx_a.read(ActiveCall::global);

    client_a
        .fs()
        .insert_tree(path!("/project"), json!({ ".git": {} }))
        .await;
    client_a
        .fs()
        .insert_branches(Path::new(path!("/project/.git")), &["main"]);

    let (project_a, _) = client_a.build_local_project(path!("/project"), cx_a).await;
    let project_id = active_call_a
        .update(cx_a, |call, cx| call.share_project(project_a.clone(), cx))
        .await
        .unwrap();
    let project_b = client_b.join_remote_project(project_id, cx_b).await;
    executor.run_until_parked();

    // Verify we can call branches() on the remote repo (proven pattern).
    let repo_b = cx_b.update(|cx| project_b.read(cx).active_repository(cx).unwrap());
    let branches = cx_b
        .update(|cx| repo_b.update(cx, |repo, _| repo.branches()))
        .await
        .unwrap()
        .unwrap();
    assert!(
        branches.iter().any(|b| b.name() == "main"),
        "should see main branch via remote"
    );

    // Pre-populate a worktree on the host so we can remove it via remote.
    // Create the directory first since remove_worktree does filesystem operations.
    client_a
        .fs()
        .create_dir(Path::new("/worktrees/test-branch"))
        .await
        .unwrap();
    client_a
        .fs()
        .with_git_state(Path::new(path!("/project/.git")), false, |state| {
            state.worktrees.push(git::repository::Worktree {
                path: PathBuf::from("/worktrees/test-branch"),
                ref_name: "refs/heads/test-branch".into(),
                sha: "abc123".into(),
            });
        })
        .unwrap();

    // Remove the worktree via the remote RPC path.
    cx_b.update(|cx| {
        repo_b.update(cx, |repo, _| {
            repo.remove_worktree(PathBuf::from("/worktrees/test-branch"), false)
        })
    })
    .await
    .unwrap()
    .unwrap();
    executor.run_until_parked();

    // Verify the worktree was removed on the host.
    client_a
        .fs()
        .with_git_state(Path::new(path!("/project/.git")), false, |state| {
            assert!(
                state.worktrees.is_empty(),
                "worktree should be removed on host"
            );
        })
        .unwrap();
}

#[gpui::test]
async fn test_repository_rename_worktree_remote_roundtrip(
    executor: BackgroundExecutor,
    cx_a: &mut TestAppContext,
    cx_b: &mut TestAppContext,
) {
    let mut server = TestServer::start(executor.clone()).await;
    let client_a = server.create_client(cx_a, "user_a").await;
    let client_b = server.create_client(cx_b, "user_b").await;
    server
        .create_room(&mut [(&client_a, cx_a), (&client_b, cx_b)])
        .await;
    let active_call_a = cx_a.read(ActiveCall::global);

    client_a
        .fs()
        .insert_tree(path!("/project"), json!({ ".git": {} }))
        .await;
    client_a
        .fs()
        .insert_branches(Path::new(path!("/project/.git")), &["main"]);

    let (project_a, _) = client_a.build_local_project(path!("/project"), cx_a).await;
    let project_id = active_call_a
        .update(cx_a, |call, cx| call.share_project(project_a.clone(), cx))
        .await
        .unwrap();
    let project_b = client_b.join_remote_project(project_id, cx_b).await;
    executor.run_until_parked();

    let repo_b = cx_b.update(|cx| project_b.read(cx).active_repository(cx).unwrap());

    // Pre-populate a worktree on the host so we can rename it via remote.
    // Create the directory first since rename_worktree does filesystem operations.
    client_a
        .fs()
        .create_dir(Path::new("/worktrees/old-branch"))
        .await
        .unwrap();
    client_a
        .fs()
        .with_git_state(Path::new(path!("/project/.git")), false, |state| {
            state.worktrees.push(git::repository::Worktree {
                path: PathBuf::from("/worktrees/old-branch"),
                ref_name: "refs/heads/old-branch".into(),
                sha: "abc123".into(),
            });
        })
        .unwrap();

    // Rename the worktree via the remote RPC path.
    cx_b.update(|cx| {
        repo_b.update(cx, |repo, _| {
            repo.rename_worktree(
                PathBuf::from("/worktrees/old-branch"),
                PathBuf::from("/worktrees/new-branch"),
            )
        })
    })
    .await
    .unwrap()
    .unwrap();
    executor.run_until_parked();

    // Verify the worktree was renamed on the host.
    client_a
        .fs()
        .with_git_state(Path::new(path!("/project/.git")), false, |state| {
            assert_eq!(state.worktrees.len(), 1, "should still have one worktree");
            assert_eq!(
                state.worktrees[0].path,
                PathBuf::from("/worktrees/new-branch"),
                "worktree path should be renamed on host"
            );
        })
        .unwrap();
}
