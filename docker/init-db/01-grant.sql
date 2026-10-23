-- The image creates sabong_root with access to sabong_lara_db only.
-- A full dump import needs database, routine, and view privileges.
GRANT ALL PRIVILEGES ON *.* TO 'sabong_root'@'%' WITH GRANT OPTION;
FLUSH PRIVILEGES;
