#![allow(missing_docs, clippy::missing_docs_in_private_items)]

#[cfg(test)]
mod integration_tests {
    use ems_industrial_gateway::app;
    #[test]
    fn test_app() {
        // arrange
        let expected = 3;
        // act
        let actual = app(1, 2);
        // assert
        assert_eq!(actual, expected);
    }
}
