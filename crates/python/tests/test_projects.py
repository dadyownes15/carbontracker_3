"""Projects and run names (docs/specifications/14-projects.md), always pinned."""

import json
import os
import sys

import pytest
from conftest import PINNED

from carbontracker import CarbonTracker, report, track

unix_only = pytest.mark.skipif(sys.platform == "win32", reason="no links on Windows")


def header(log_dir):
    """The run header of the only record under ``log_dir``."""
    [path] = log_dir.glob("*/*.ct")
    return json.loads(path.read_text().splitlines()[0])


def projects(tmp_path):
    return tmp_path / "data" / "carbontracker" / "projects"


# AC/PRJ-2, AC/PRJ-3, AC/PRJ-6
def test_the_header_holds_the_project_and_name_given(tmp_path):
    CarbonTracker(log_dir=tmp_path / "logs", project="saintslab/codeq", name="sweep bits 4", **PINNED).stop()
    run = header(tmp_path / "logs")
    assert (run["project"], run["name"]) == ("saintslab/codeq", "sweep bits 4")


# AC/PRJ-6
def test_a_run_without_project_or_name_has_no_such_field(tmp_path):
    CarbonTracker(log_dir=tmp_path / "logs", **PINNED).stop()
    run = header(tmp_path / "logs")
    assert "project" not in run and "name" not in run


# AC/PRJ-2, AC/PRJ-3
def test_track_takes_the_project_and_name(tmp_path):
    @track(log_dir=tmp_path / "logs", project="codeq", name="eval", **PINNED)
    def evaluate():
        return 1

    evaluate()
    run = header(tmp_path / "logs")
    assert (run["project"], run["name"]) == ("codeq", "eval")


# AC/PRJ-4
@pytest.mark.parametrize(
    "project", ["", "/codeq", "codeq/", "a//b", "../escape", ".hidden", "-dash", "with space", "tab\there", "é", 3]
)
def test_a_project_it_cannot_take_raises_before_probing(project):
    with pytest.raises(ValueError, match="project"):
        CarbonTracker(project=project, ignore_errors=True, **PINNED)


# AC/PRJ-4
@pytest.mark.parametrize("project", ["codeq", "saintslab/codeq", "a.b_c-d/E9", "9lives"])
def test_a_project_of_parts_is_taken(project):
    CarbonTracker(project=project, **PINNED).stop()


# AC/PRJ-5
@pytest.mark.parametrize("name", ["", "   ", "two\nlines", "bell\a", 3])
def test_a_name_it_cannot_take_raises_before_probing(name):
    with pytest.raises(ValueError, match="name"):
        CarbonTracker(name=name, ignore_errors=True, **PINNED)


# AC/PRJ-7, AC/PRJ-8
@unix_only
def test_a_run_is_linked_from_its_projects_folder_when_it_starts(tmp_path):
    tracker = CarbonTracker(log_dir=tmp_path / "logs", project="saintslab/codeq", **PINNED)
    [folder] = (tmp_path / "logs").iterdir()
    link = projects(tmp_path) / "saintslab" / "codeq" / folder.name
    assert link.is_symlink()
    assert os.readlink(link) == str(folder.resolve())
    tracker.stop()


# AC/PRJ-7
@unix_only
def test_without_xdg_data_home_the_folder_is_under_the_home(tmp_path, monkeypatch):
    monkeypatch.delenv("XDG_DATA_HOME")
    monkeypatch.setenv("HOME", str(tmp_path / "home"))
    CarbonTracker(log_dir=tmp_path / "logs", project="codeq", **PINNED).stop()
    [folder] = (tmp_path / "logs").iterdir()
    assert (tmp_path / "home" / ".local" / "share" / "carbontracker" / "projects" / "codeq" / folder.name).is_symlink()


# AC/PRJ-9
def test_a_run_without_a_folder_links_nothing(tmp_path):
    CarbonTracker(log_dir=None, project="codeq", **PINNED).stop()
    assert not (tmp_path / "data").exists()


# AC/PRJ-10
@unix_only
def test_a_link_that_cannot_be_made_is_a_warning_and_tracking_goes_on(tmp_path, capsys):
    (tmp_path / "data").write_text("a file where the data folder goes")
    tracker = CarbonTracker(log_dir=tmp_path / "logs", project="codeq", **PINNED)
    session = tracker.stop()
    assert session is not None and session.total.energy_kwh > 0
    err = capsys.readouterr().err
    assert "warning" in err.lower() and str(projects(tmp_path) / "codeq") in err


# AC/PRJ-21
@unix_only
def test_report_reads_a_projects_runs_from_its_folder(tmp_path):
    CarbonTracker(log_dir=tmp_path / "a", project="codeq", name="first", **PINNED).stop()
    CarbonTracker(log_dir=tmp_path / "b", project="codeq", **PINNED).stop()
    CarbonTracker(log_dir=tmp_path / "c", project="paper", **PINNED).stop()
    found = report(project="codeq")
    assert found.total.runs == 2
    assert sorted((run.project, run.name or "") for run in found.runs) == [("codeq", ""), ("codeq", "first")]


# AC/PRJ-21
def test_report_with_paths_keeps_only_the_projects_runs(tmp_path):
    CarbonTracker(log_dir=tmp_path / "logs", project="codeq", **PINNED).stop()
    CarbonTracker(log_dir=tmp_path / "logs", project="paper", **PINNED).stop()
    CarbonTracker(log_dir=tmp_path / "logs", **PINNED).stop()
    found = report(tmp_path / "logs", project="codeq")
    assert [run.project for run in found.runs] == ["codeq"]
    assert found.total.runs == 1


# AC/PRJ-21
def test_report_on_a_project_with_no_folder_raises(tmp_path):
    with pytest.raises(FileNotFoundError, match="nothing"):
        report(project="nothing")
