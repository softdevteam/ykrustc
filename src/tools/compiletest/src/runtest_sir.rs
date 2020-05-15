// Bits for running Yorick SIR tests.
//
// This code is based on an older version of the mir-opt running code, which still had support for
// elision in expected outcomes.
//
// This is designed to be `include!()`ed from runtest.rs so as to retain the original crate
// structure.

#[derive(Clone, PartialEq, Eq, Debug)]
enum ExpectedLine<T: AsRef<str>> {
    Elision,
    Text(T),
}

impl<'test> TestCx<'test> {
    fn run_yk_sir_test(&self) {
        let proc_res = self.compile_test(WillExecute::No, EmitMetadata::No);

        if !proc_res.status.success() {
            self.fatal_proc_rec("compilation failed!", &proc_res);
        }

        self.check_yk_sir_dump();
    }

    fn check_yk_sir_dump(&self) {
        let test_file_contents = fs::read_to_string(&self.testpaths.file).unwrap();
        if let Some(idx) = test_file_contents.find("// END RUST SOURCE") {
            let (_, test_text) = test_file_contents.split_at(idx + "// END_RUST SOURCE".len());
            let mut test_lines = vec![ExpectedLine::Elision];
            for l in test_text.lines() {
                if l.is_empty() {
                    // ignore
                } else if l.starts_with("//") && l.split_at("//".len()).1.trim() == "..." {
                    test_lines.push(ExpectedLine::Elision)
                } else if l.starts_with("// ") {
                    let (_, test_content) = l.split_at("// ".len());
                    test_lines.push(ExpectedLine::Text(test_content));
                }
            }
            // From here on out, we are re-using parts of the `MirOpt` test suite's matcher.
            let output_path = self.output_base_name().with_extension("yksir");
            self.compare_yk_sir_test_output(output_path.to_str().unwrap(), &test_lines);
        } else {
            panic!("no expected outcome in test file!");
        }
    }

    fn compare_yk_sir_test_output(&self, test_name: &str, expected_content: &[ExpectedLine<&str>]) {
        let mut output_file = PathBuf::new();
        output_file.push(self.get_mir_dump_dir());
        output_file.push(test_name);
        debug!("comparing the contests of: {:?}", output_file);
        debug!("with: {:?}", expected_content);
        if !output_file.exists() {
            panic!(
                "Output file `{}` from test does not exist",
                output_file.into_os_string().to_string_lossy()
            );
        }
        self.check_mir_test_timestamp(test_name, &output_file);

        let dumped_string = fs::read_to_string(&output_file).unwrap();
        let mut dumped_lines =
            dumped_string.lines().map(|l| nocomment_sir_line(l)).filter(|l| !l.is_empty());
        let mut expected_lines = expected_content
            .iter()
            .filter(|&l| if let &ExpectedLine::Text(l) = l { !l.is_empty() } else { true })
            .peekable();

        let compare = |expected_line, dumped_line| {
            let e_norm = normalize_sir_line(expected_line);
            let d_norm = normalize_sir_line(dumped_line);
            debug!("found: {:?}", d_norm);
            debug!("expected: {:?}", e_norm);
            e_norm == d_norm
        };

        let error = |expected_line, extra_msg| {
            let normalize_all = dumped_string
                .lines()
                .map(nocomment_sir_line)
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            let f = |l: &ExpectedLine<_>| match l {
                &ExpectedLine::Elision => "... (elided)".into(),
                &ExpectedLine::Text(t) => t,
            };
            let expected_content =
                expected_content.iter().map(|l| f(l)).collect::<Vec<_>>().join("\n");
            panic!(
                "Did not find expected line, error: {}\n\
                 Expected Line: {:?}\n\
                 Test Name: {}\n\
                 Expected:\n{}\n\
                 Actual:\n{}",
                extra_msg, expected_line, test_name, expected_content, normalize_all
            );
        };

        // We expect each non-empty line to appear consecutively, non-consecutive lines
        // must be separated by at least one Elision
        let mut start_block_line = None;
        while let Some(dumped_line) = dumped_lines.next() {
            match expected_lines.next() {
                Some(&ExpectedLine::Text(expected_line)) => {
                    let normalized_expected_line = normalize_sir_line(expected_line);
                    if normalized_expected_line.contains(":{") {
                        start_block_line = Some(expected_line);
                    }

                    if !compare(expected_line, dumped_line) {
                        error!("{:?}", start_block_line);
                        error(
                            expected_line,
                            format!(
                                "Mismatch in lines\n\
                                 Current block: {}\n\
                                 Actual Line: {:?}",
                                start_block_line.unwrap_or("None"),
                                dumped_line
                            ),
                        );
                    }
                }
                Some(&ExpectedLine::Elision) => {
                    // skip any number of elisions in a row.
                    while let Some(&&ExpectedLine::Elision) = expected_lines.peek() {
                        expected_lines.next();
                    }
                    if let Some(&ExpectedLine::Text(expected_line)) = expected_lines.next() {
                        let mut found = compare(expected_line, dumped_line);
                        if found {
                            continue;
                        }
                        while let Some(dumped_line) = dumped_lines.next() {
                            found = compare(expected_line, dumped_line);
                            if found {
                                break;
                            }
                        }
                        if !found {
                            error(expected_line, "ran out of mir dump to match against".into());
                        }
                    }
                }
                None => {}
            }
        }
    }
}

fn normalize_sir_line(line: &str) -> String {
    nocomment_sir_line(line).replace(char::is_whitespace, "")
}

fn nocomment_sir_line(line: &str) -> &str {
    if let Some(idx) = line.find("//") {
        let (l, _) = line.split_at(idx);
        l.trim_end()
    } else {
        line
    }
}
