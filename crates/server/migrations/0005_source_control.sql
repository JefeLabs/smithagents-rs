ALTER TABLE repos ADD COLUMN source_control TEXT;
UPDATE repos SET source_control =
    json_object('git', json_object('default_branch', default_branch));
ALTER TABLE repos DROP COLUMN default_branch;
