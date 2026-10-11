use super::*;
use crate::store::test_support::TestData;

const PROBE: &str = r#"
set -e
cat "$1/POLICY.md"
test -s "$1/upstream/ponytail/skills/ponytail/SKILL.md"
test -s "$1/upstream/pstack/skills/principle-prove-it-works/SKILL.md"
test -s "$1/upstream/cursor-team-kit/skills/thermo-nuclear-code-quality-review/SKILL.md"
! (printf changed > "$1/POLICY.md")
! mkdir "$1/project-override"
! chmod u+w "$1/POLICY.md"
"#;

fn codex_probe(mut command: Command, cwd: &Path, profile: &str, root: &Path) -> io::Result<()> {
    let (mut child, mut rpc) = Rpc::start(&mut command)?;
    let result = (|| {
        rpc.call("initialize", json!({"clientInfo":{"name":"skill_boundary_test","version":"1"},"capabilities":{"experimentalApi":true}}))?;
        rpc.write(json!({"method":"initialized"}))?;
        let output = rpc.call("command/exec", json!({"command":["/bin/sh","-c",PROBE,"skills",root],"cwd":cwd,"permissionProfile":profile,"timeoutMs":10000}))?;
        assert_eq!(output["exitCode"], 0, "{output}");
        assert!(
            output["stdout"]
                .as_str()
                .unwrap()
                .contains("Sprowt skill compatibility policy")
        );
        Ok(())
    })();
    rpc::terminate(&mut child);
    result
}

#[test]
#[ignore = "checks embedded skill files and read-only planner/Codex/Muse profiles in a disposable VM; no inference"]
fn pinned_skills_are_read_only_for_host_and_vm_agents() {
    let data = TestData::new();
    let project = data.0.join("project");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("source.txt"), "unchanged").unwrap();
    let host_skills = crate::skills::prepare_host().unwrap();
    let mut host = Command::new("codex");
    host.args(["app-server", "--listen", "stdio://"])
        .current_dir(&project);
    for config in crate::codex::configuration(&project).unwrap() {
        host.args(["-c", &config]);
    }
    codex_probe(host, &project, "sprowt_readonly", &host_skills).unwrap();
    let root = data.0.join("skills-vm");
    workspace::create(&project, &root).unwrap();
    let flag = AtomicBool::new(false);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> io::Result<()> {
        let mut vm = Sandbox::prepare(&root, &flag, |label| eprintln!("{label}"))?;
        let skills = crate::skills::guest_root();
        vm.prepare_skills(&flag)?;
        vm.prepare_skills(&flag)?;
        // An interrupted controller install is repaired without touching task source.
        vm.guest(&["/bin/rm", &format!("{skills}/.ready")], &flag)?;
        vm.prepare_skills(&flag)?;
        vm.prepare_tasks(&[1], &flag)?;
        vm.assign_muse_task(1, 1, &flag)?;
        let output = container()
            .args([
                "exec",
                &vm.name,
                "env",
                "-i",
                "HOME=/home/sprowt/workers/1",
                "CODEX_HOME=/opt/codex-home",
                "PATH=/usr/local/bin:/usr/bin:/bin",
                "/usr/local/bin/codex",
                "sandbox",
                "--permission-profile",
                "sprowt_task_1",
                "-c",
                &vm.profile("sprowt_task_1", "/tasks/1", "/home/sprowt/workers/1"),
                "--cd",
                "/tasks/1",
                "--",
                "/bin/sh",
                "-c",
                PROBE,
                "skills",
                &skills,
            ])
            .output()?;
        checked(&output)?;
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("Sprowt skill compatibility policy")
        );

        let config = vm.muse_configuration(1);
        let rpc = vm.rpc.as_mut().unwrap();
        let process = "muse-skills";
        let start = rpc.call("process/start", json!({"processId":process,"argv":["/bin/sh","-c",PROBE,"skills",skills],"cwd":"file:///tasks/1","env":config["env"],"envPolicy":{"inherit":"none","ignoreDefaultExcludes":true,"exclude":[],"set":{},"includeOnly":[]},"tty":false,"arg0":null,"sandbox":config["sandbox"],"enforceManagedNetwork":true,"networkProxy":config["proxy"]}))?;
        assert_eq!(start["sandboxType"], "linuxSeccomp");
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut cursor = 0;
        let mut output = Vec::new();
        loop {
            assert!(Instant::now() < deadline, "Muse skill probe timed out");
            let read = rpc.call(
                "process/read",
                json!({"processId":process,"afterSeq":cursor,"maxBytes":65536,"waitMs":100}),
            )?;
            for chunk in read["chunks"].as_array().unwrap() {
                output.extend(BASE64.decode(chunk["chunk"].as_str().unwrap()).unwrap());
            }
            cursor = read["nextSeq"].as_u64().unwrap().saturating_sub(1);
            if read["closed"] == true && read["chunks"].as_array().unwrap().is_empty() {
                assert_eq!(read["exitCode"], 0, "{}", String::from_utf8_lossy(&output));
                assert!(
                    String::from_utf8_lossy(&output).contains("Sprowt skill compatibility policy")
                );
                break;
            }
        }
        assert_eq!(fs::read(root.join("work/source.txt"))?, b"unchanged");
        assert!(!root.join("work/skills").exists());
        drop(vm);
        let vm = Sandbox::prepare(&root, &flag, |_| {})?;
        vm.prepare_skills(&flag)?;
        assert!(vm.guest_exists(&format!("{skills}/.ready"))?);
        Ok(())
    }));
    if root.join("vm.json").exists() {
        delete(&root).unwrap();
    }
    result.unwrap().unwrap();
}
