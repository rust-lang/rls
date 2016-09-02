curl -v -H "Content-Type: application/json" -X POST -d '{{"pos": {"filepath":"sample_project/src/main.rs","line":22,"col":5}, "span":{"file_name":"sample_project/src/main.rs","line_start":22,"column_start":5,"line_end":22,"column_end":6}}}' 127.0.0.1:9000/goto_def

